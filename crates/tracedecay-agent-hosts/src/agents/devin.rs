//! Devin agent integration.
//!
//! Devin discovers stdio MCP servers from a dedicated configuration
//! document. TraceDecay owns only the `mcpServers.tracedecay` entry and leaves
//! every other server untouched. Current Devin releases use
//! `~/.config/devin/mcp_config.json` for user scope and
//! `<project>/.devin/mcp_config.json` for shared project scope; older main
//! config entries are migrated by Devin itself.

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::errors::{Result, TraceDecayError};

use super::host_bundle_v2::HostBundleRegistrationStateV1;
use super::{
    AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext, JsonConfigDialect,
    McpDoctorLabels, TextFileMutation, config_backup_path, doctor_check_mcp_registration,
    load_json_file, update_config_file_transactionally,
};

pub struct DevinIntegration;

fn devin_config_dir(home: &Path) -> PathBuf {
    home.join(".config/devin")
}

/// Current user-scoped MCP configuration path documented by Devin.
fn devin_mcp_config_path(home: &Path) -> PathBuf {
    devin_config_dir(home).join("mcp_config.json")
}

/// Current project-scoped MCP configuration path documented by Devin.
fn devin_project_mcp_config_path(project_path: &Path) -> PathBuf {
    project_path.join(".devin/mcp_config.json")
}

fn devin_original_config_path(config_path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.tracedecay-original", config_path.display()))
}

impl AgentIntegration for DevinIntegration {
    fn name(&self) -> &'static str {
        "Devin"
    }

    fn id(&self) -> &'static str {
        "devin"
    }

    fn supports_local_install(&self) -> bool {
        true
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mDevin integration\x1b[0m");
        doctor_check_mcp_registration(
            dc,
            &devin_mcp_config_path(&ctx.home),
            "mcpServers",
            load_json_file,
            &McpDoctorLabels {
                agent_id: "devin",
                product: "Devin user configuration",
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
                    product: "Devin project configuration",
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
            vec![
                path.clone(),
                config_backup_path(&path),
                devin_original_config_path(&path),
            ]
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
            Ok(vec![
                path.clone(),
                config_backup_path(&path),
                devin_original_config_path(&path),
            ])
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
        let config_path = devin_project_mcp_config_path(project_path);
        let original_path = devin_original_config_path(&config_path);
        super::ensure_project_local_safe_paths(
            project_path,
            [config_path.as_path(), original_path.as_path()],
        )?;
        install_mcp_if_selected(components, &config_path, ctx)
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
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| TraceDecayError::Config {
                message: format!(
                    "cannot create Devin config directory {}: {error}",
                    parent.display()
                ),
            })?;
        }
        let original_path = devin_original_config_path(config_path);
        update_config_file_transactionally(config_path, |existing| {
            let mut settings = JsonConfigDialect::Json.parse_for_edit(config_path, existing)?;
            if !settings.is_object() {
                return Err(TraceDecayError::Config {
                    message: format!("{} must contain a JSON object", config_path.display()),
                });
            }
            if settings
                .get("mcpServers")
                .is_some_and(|value| !value.is_object())
            {
                return Err(TraceDecayError::Config {
                    message: format!("{}.mcpServers must be a JSON object", config_path.display()),
                });
            }
            let has_tracedecay = settings.pointer("/mcpServers/tracedecay").is_some();
            if !has_tracedecay && config_path.is_file() && !original_path.exists() {
                super::safe_write_bytes_file(&original_path, existing.as_bytes(), None)?;
            }
            settings["mcpServers"]["tracedecay"] = json!({
                "command": ctx.tracedecay_bin.clone(),
                "args": ["serve"],
                "env": {},
                "transport": "stdio",
            });
            Ok((
                (),
                TextFileMutation::Write(super::render_json_config(config_path, &settings)?),
            ))
        })?;
        eprintln!(
            "\x1b[32m✔\x1b[0m Added tracedecay MCP server to {}",
            config_path.display()
        );
    }
    Ok(())
}

enum DevinMcpRemoval {
    NoEntry,
    RestoredOriginal,
    Rewritten,
}

fn uninstall_mcp_if_selected(
    components: &[super::host_bundle_v2::HostBundleComponentV1],
    config_path: &Path,
) -> Result<()> {
    if components.contains(&super::host_bundle_v2::HostBundleComponentV1::ContextMcp) {
        if !config_path.exists() {
            eprintln!("  {} not found, skipping", config_path.display());
            return Ok(());
        }
        let original_path = devin_original_config_path(config_path);
        let outcome = update_config_file_transactionally(config_path, |existing| {
            let mut settings = JsonConfigDialect::Json.parse_for_edit(config_path, existing)?;
            let Some(servers) = settings
                .get_mut("mcpServers")
                .and_then(serde_json::Value::as_object_mut)
            else {
                return Ok((DevinMcpRemoval::NoEntry, TextFileMutation::Unchanged));
            };
            if servers.remove("tracedecay").is_none() {
                return Ok((DevinMcpRemoval::NoEntry, TextFileMutation::Unchanged));
            }
            if let Ok(original) = std::fs::read(&original_path)
                && serde_json::from_slice::<serde_json::Value>(&original).ok()
                    == Some(settings.clone())
            {
                let original =
                    String::from_utf8(original).map_err(|error| TraceDecayError::Config {
                        message: format!("{} is not valid UTF-8: {error}", original_path.display()),
                    })?;
                return Ok((
                    DevinMcpRemoval::RestoredOriginal,
                    TextFileMutation::Write(original),
                ));
            }
            Ok((
                DevinMcpRemoval::Rewritten,
                TextFileMutation::Write(super::render_json_config(config_path, &settings)?),
            ))
        })?;
        match outcome {
            DevinMcpRemoval::NoEntry => eprintln!(
                "  No tracedecay MCP server in {}, skipping",
                config_path.display()
            ),
            DevinMcpRemoval::RestoredOriginal => {
                super::safe_remove_host_file(&original_path).map_err(|error| {
                    TraceDecayError::Config {
                        message: format!("failed to remove {}: {error}", original_path.display()),
                    }
                })?;
                eprintln!(
                    "\x1b[32m✔\x1b[0m Restored original Devin configuration in {}",
                    config_path.display()
                );
            }
            DevinMcpRemoval::Rewritten => eprintln!(
                "\x1b[32m✔\x1b[0m Removed tracedecay MCP server from {}",
                config_path.display()
            ),
        }
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
        let original = br#"{"mcpServers":{"other":{"command":"other-mcp"}},"ui":{"theme":"dark"}}"#;
        std::fs::write(&config, original).unwrap();
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
        assert_eq!(
            std::fs::read(devin_original_config_path(&config)).unwrap(),
            original
        );
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
        assert_eq!(std::fs::read(&config).unwrap(), original);
        assert!(!devin_original_config_path(&config).exists());
    }
}
