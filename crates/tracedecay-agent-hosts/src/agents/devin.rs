//! Devin Local agent integration.
//!
//! Devin Local discovers stdio MCP servers from a dedicated configuration
//! document. TraceDecay owns only the `mcpServers.tracedecay` entry and leaves
//! every other server untouched. Current Devin Local releases use
//! `~/.config/devin/mcp_config.json` for user scope and
//! `<project>/.devin/mcp_config.json` for shared project scope; older main
//! config entries are migrated by Devin itself.

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::errors::Result;

use super::host_bundle_v2::HostBundleRegistrationStateV1;
use super::{
    AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext, JsonConfigDialect,
    McpDoctorLabels, McpUninstallPolicy, config_backup_path, doctor_check_mcp_registration,
    install_mcp_server_entry, load_json_file, uninstall_mcp_server_entry,
};

pub struct DevinIntegration;

fn devin_config_dir(home: &Path) -> PathBuf {
    home.join(".config/devin")
}

/// Current user-scoped MCP configuration path documented by Devin Local.
fn devin_mcp_config_path(home: &Path) -> PathBuf {
    devin_config_dir(home).join("mcp_config.json")
}

/// Current project-scoped MCP configuration path documented by Devin Local.
fn devin_project_mcp_config_path(project_path: &Path) -> PathBuf {
    project_path.join(".devin/mcp_config.json")
}

impl AgentIntegration for DevinIntegration {
    fn name(&self) -> &'static str {
        "Devin Local"
    }

    fn id(&self) -> &'static str {
        "devin"
    }

    fn supports_local_install(&self) -> bool {
        true
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mDevin Local integration\x1b[0m");
        doctor_check_mcp_registration(
            dc,
            &devin_mcp_config_path(&ctx.home),
            "mcpServers",
            load_json_file,
            &McpDoctorLabels {
                agent_id: "devin",
                product: "Devin Local user configuration",
                registered: "MCP server registered",
                missing: "MCP server NOT registered",
            },
        );
        let project_config = devin_project_mcp_config_path(&ctx.project_path);
        if project_config.exists() {
            doctor_check_mcp_registration(
                dc,
                &project_config,
                "mcpServers",
                load_json_file,
                &McpDoctorLabels {
                    agent_id: "devin",
                    product: "Devin Local project configuration",
                    registered: "project MCP server registered",
                    missing: "project MCP server NOT registered",
                },
            );
        }
    }

    fn host_component_registration(
        &self,
        component: super::host_bundle_v2::HostBundleComponentV1,
        ctx: &HealthcheckContext,
    ) -> super::host_bundle_v2::HostBundleRegistrationStateV1 {
        if component != super::host_bundle_v2::HostBundleComponentV1::ContextMcp {
            return super::host_bundle_v2::HostBundleRegistrationStateV1::Missing;
        }
        devin_mcp_registration_state(&devin_mcp_config_path(&ctx.home))
    }

    fn is_detected(&self, home: &Path) -> bool {
        devin_config_dir(home).is_dir()
    }

    fn primary_config_path(&self, home: &Path) -> Option<PathBuf> {
        Some(devin_mcp_config_path(home))
    }

    fn host_component_registration_paths(
        &self,
        components: &[super::host_bundle_v2::HostBundleComponentV1],
        home: &Path,
    ) -> Vec<PathBuf> {
        if components == [super::host_bundle_v2::HostBundleComponentV1::ContextMcp] {
            let path = devin_mcp_config_path(home);
            vec![path.clone(), config_backup_path(&path)]
        } else {
            Vec::new()
        }
    }

    fn project_host_component_registration_paths(
        &self,
        components: &[super::host_bundle_v2::HostBundleComponentV1],
        _home: &Path,
        project_path: &Path,
    ) -> Result<Vec<PathBuf>> {
        if components == [super::host_bundle_v2::HostBundleComponentV1::ContextMcp] {
            let path = devin_project_mcp_config_path(project_path);
            Ok(vec![path.clone(), config_backup_path(&path)])
        } else {
            Ok(Vec::new())
        }
    }

    #[hotpath::measure(label = "devin_mcp_install")]
    fn activate_deployed_host_component_registration(
        &self,
        components: &[super::host_bundle_v2::HostBundleComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        install_mcp_if_selected(components, &devin_mcp_config_path(&ctx.home), ctx)
    }

    fn deactivate_deployed_host_component_registration(
        &self,
        components: &[super::host_bundle_v2::HostBundleComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        uninstall_mcp_if_selected(components, &devin_mcp_config_path(&ctx.home))
    }

    fn activate_project_host_component_registration(
        &self,
        components: &[super::host_bundle_v2::HostBundleComponentV1],
        ctx: &InstallContext,
        project_path: &Path,
    ) -> Result<()> {
        super::ensure_project_local_safe_paths(
            project_path,
            [devin_project_mcp_config_path(project_path).as_path()],
        )?;
        install_mcp_if_selected(
            components,
            &devin_project_mcp_config_path(project_path),
            ctx,
        )
    }

    fn deactivate_project_host_component_registration(
        &self,
        components: &[super::host_bundle_v2::HostBundleComponentV1],
        _ctx: &InstallContext,
        project_path: &Path,
    ) -> Result<()> {
        uninstall_mcp_if_selected(components, &devin_project_mcp_config_path(project_path))
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        super::mcp_config_has_tracedecay(&devin_mcp_config_path(home), "mcpServers", load_json_file)
    }
}

/// Devin treats an omitted `disabled` field as enabled. Its documented MCP
/// examples omit the field, so this adapter cannot use the stricter shared
/// state reader used by hosts that require an explicit `disabled: false`.
fn devin_mcp_registration_state(config_path: &Path) -> HostBundleRegistrationStateV1 {
    let Ok(bytes) = std::fs::read(config_path) else {
        return HostBundleRegistrationStateV1::Missing;
    };
    let Ok(settings) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return HostBundleRegistrationStateV1::Corrupt;
    };
    let Some(server) = settings.pointer("/mcpServers/tracedecay") else {
        return HostBundleRegistrationStateV1::Missing;
    };
    let command_is_present = server
        .get("command")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|command| !command.is_empty());
    let serves_tracedecay = server
        .get("args")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|args| args.iter().any(|arg| arg.as_str() == Some("serve")));
    let disabled = server
        .get("disabled")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if server.is_object() && command_is_present && serves_tracedecay && !disabled {
        HostBundleRegistrationStateV1::Current
    } else {
        HostBundleRegistrationStateV1::Missing
    }
}

fn install_mcp_if_selected(
    components: &[super::host_bundle_v2::HostBundleComponentV1],
    config_path: &Path,
    ctx: &InstallContext,
) -> Result<()> {
    if components.contains(&super::host_bundle_v2::HostBundleComponentV1::ContextMcp) {
        install_mcp_server_entry(
            config_path,
            "mcpServers",
            json!({
                "command": ctx.tracedecay_bin.clone(),
                "args": ["serve"],
                "env": {},
            }),
            "Devin Local",
            JsonConfigDialect::Json,
        )?;
    }
    Ok(())
}

fn uninstall_mcp_if_selected(
    components: &[super::host_bundle_v2::HostBundleComponentV1],
    config_path: &Path,
) -> Result<()> {
    if components.contains(&super::host_bundle_v2::HostBundleComponentV1::ContextMcp) {
        uninstall_mcp_server_entry(
            config_path,
            "mcpServers",
            JsonConfigDialect::Json,
            McpUninstallPolicy::default(),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_devin_paths_distinguish_user_and_project_scope() {
        let home = Path::new("/tmp/home");
        let project = Path::new("/tmp/project");
        assert_eq!(
            devin_mcp_config_path(home),
            PathBuf::from("/tmp/home/.config/devin/mcp_config.json")
        );
        assert_eq!(
            devin_project_mcp_config_path(project),
            PathBuf::from("/tmp/project/.devin/mcp_config.json")
        );
    }

    #[test]
    fn documented_server_entry_is_current_without_disabled_field() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("mcp_config.json");
        std::fs::write(
            &config,
            r#"{"mcpServers":{"tracedecay":{"command":"/usr/local/bin/tracedecay","args":["serve"],"env":{}}}}"#,
        )
        .unwrap();

        assert_eq!(
            devin_mcp_registration_state(&config),
            HostBundleRegistrationStateV1::Current
        );
    }

    #[test]
    fn project_lifecycle_preserves_foreign_devin_configuration() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let config = devin_project_mcp_config_path(project.path());
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(
            &config,
            r#"{"mcpServers":{"other":{"command":"other-mcp"}},"ui":{"theme":"dark"}}"#,
        )
        .unwrap();
        let components = [super::super::host_bundle_v2::HostBundleComponentV1::ContextMcp];
        let install = InstallContext {
            home: home.path().to_path_buf(),
            tracedecay_bin: "/tmp/tracedecay-a".to_string(),
            tool_permissions: Vec::new(),
            project_root: Some(project.path().to_path_buf()),
            dashboard: false,
        };

        DevinIntegration
            .activate_project_host_component_registration(&components, &install, project.path())
            .unwrap();
        let installed = load_json_file(&config);
        assert_eq!(installed["ui"]["theme"], "dark");
        assert_eq!(installed["mcpServers"]["other"]["command"], "other-mcp");
        assert_eq!(
            installed["mcpServers"]["tracedecay"]["command"],
            "/tmp/tracedecay-a"
        );

        DevinIntegration
            .deactivate_project_host_component_registration(&components, &install, project.path())
            .unwrap();
        let removed = load_json_file(&config);
        assert_eq!(removed["ui"]["theme"], "dark");
        assert_eq!(removed["mcpServers"]["other"]["command"], "other-mcp");
        assert!(removed["mcpServers"].get("tracedecay").is_none());
    }
}
