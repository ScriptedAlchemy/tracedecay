//! Google Antigravity (formerly Windsurf) agent integration.
//!
//! Handles registration of the tracedecay MCP server in:
//!
//! - `~/.gemini/antigravity/mcp_config.json` — the Antigravity IDE config,
//!   shape `{"mcpServers": {"tracedecay": {...}}}`.
//! - `~/.gemini/antigravity-cli/plugins/tracedecay.json` — the Antigravity
//!   CLI (`agy`) plugin file, same shape. Required because the IDE config
//!   is not picked up by the CLI (#85).
//!
//! `doctor` checks both locations and reports them separately.
//!
//! **Manual by necessity, not by preference (verified 2026-08-08).** The owner
//! policy is CLI-first, so these two config writes need a justification. The
//! `agy` CLI has a plugin/marketplace layer (`agy plugin list|install|disable`)
//! but no MCP command at all: Antigravity's own documentation directs users to
//! the interactive `/mcp` overlay or to editing `mcp_config.json` by hand, and
//! no `agy mcp add`/`remove` exists. The plugin commands cannot carry an MCP
//! server registration, so neither of the two files below has a command to
//! drive. See <https://antigravity.google/docs/mcp> and
//! <https://antigravity.google/docs/cli/plugins>.

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::errors::Result;

use super::{
    AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext, JsonConfigDialect,
    McpDoctorLabels, McpUninstallPolicy, config_backup_path, doctor_check_mcp_registration,
    install_mcp_server_entry, load_json_file, uninstall_mcp_server_entry,
};
use super::host_bundle_v2::{HostBundleComponentV1, HostBundleRegistrationStateV1};

pub struct AntigravityIntegration;

fn mcp_config_path(home: &Path) -> std::path::PathBuf {
    home.join(".gemini/antigravity/mcp_config.json")
}

/// Per-plugin file used by the Antigravity CLI. Holds the same shape as
/// the IDE config.
fn cli_plugin_path(home: &Path) -> std::path::PathBuf {
    home.join(".gemini/antigravity-cli/plugins/tracedecay.json")
}

impl AgentIntegration for AntigravityIntegration {
    fn name(&self) -> &'static str {
        "Antigravity"
    }

    fn id(&self) -> &'static str {
        "antigravity"
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mAntigravity integration\x1b[0m");
        doctor_check_settings(dc, &ctx.home);
        doctor_check_cli_plugin(dc, &ctx.home);
    }

    fn reports_absence_to_doctor(&self) -> bool {
        true
    }

    fn host_component_registration(
        &self,
        component: HostBundleComponentV1,
        ctx: &HealthcheckContext,
    ) -> HostBundleRegistrationStateV1 {
        antigravity_registration_state(component, &ctx.home, None)
    }

    fn host_component_registration_for_lifecycle(
        &self,
        component: HostBundleComponentV1,
        ctx: &HealthcheckContext,
        install: &InstallContext,
    ) -> HostBundleRegistrationStateV1 {
        antigravity_registration_state(component, &ctx.home, Some(&install.tracedecay_bin))
    }

    fn is_detected(&self, home: &Path) -> bool {
        home.join(".gemini/antigravity").is_dir() || home.join(".gemini/antigravity-cli").is_dir()
    }

    fn primary_config_path(&self, home: &Path) -> Option<std::path::PathBuf> {
        Some(mcp_config_path(home))
    }

    fn host_component_registration_paths(
        &self,
        components: &[HostBundleComponentV1],
        home: &Path,
    ) -> Vec<PathBuf> {
        if components == [HostBundleComponentV1::ContextMcp] {
            let ide = mcp_config_path(home);
            let cli = cli_plugin_path(home);
            vec![
                ide.clone(),
                config_backup_path(&ide),
                cli.clone(),
                config_backup_path(&cli),
            ]
        } else {
            Vec::new()
        }
    }

    fn activate_deployed_host_component_registration(
        &self,
        components: &[HostBundleComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        if components.contains(&HostBundleComponentV1::ContextMcp) {
            for (path, product) in [
                (mcp_config_path(&ctx.home), "Antigravity IDE"),
                (cli_plugin_path(&ctx.home), "Antigravity CLI"),
            ] {
                install_mcp_server_entry(
                    &path,
                    "mcpServers",
                    json!({
                        "command": ctx.tracedecay_bin.clone(),
                        "args": ["serve"],
                    }),
                    product,
                    JsonConfigDialect::Json,
                )?;
            }
        }
        Ok(())
    }

    fn deactivate_deployed_host_component_registration(
        &self,
        components: &[HostBundleComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        if components.contains(&HostBundleComponentV1::ContextMcp) {
            for path in [mcp_config_path(&ctx.home), cli_plugin_path(&ctx.home)] {
                uninstall_mcp_server_entry(
                    &path,
                    "mcpServers",
                    JsonConfigDialect::Json,
                    McpUninstallPolicy::default(),
                )?;
            }
        }
        Ok(())
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        super::mcp_config_has_tracedecay(&mcp_config_path(home), "mcpServers", load_json_file)
            && super::mcp_config_has_tracedecay(
                &cli_plugin_path(home),
                "mcpServers",
                load_json_file,
            )
    }
}

fn registration_state(path: &Path, expected_binary: Option<&str>) -> HostBundleRegistrationStateV1 {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return HostBundleRegistrationStateV1::Missing;
        }
        Err(_) => return HostBundleRegistrationStateV1::Corrupt,
    };
    let settings = match serde_json::from_slice::<serde_json::Value>(&bytes) {
        Ok(settings) if settings.is_object() => settings,
        _ => return HostBundleRegistrationStateV1::Corrupt,
    };
    let Some(server) = settings.pointer("/mcpServers/tracedecay") else {
        return HostBundleRegistrationStateV1::Missing;
    };
    let command_is_current = server
        .get("command")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|command| {
            expected_binary.map_or_else(|| !command.is_empty(), |expected| command == expected)
        });
    let serves = server
        .get("args")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|args| args.iter().any(|arg| arg.as_str() == Some("serve")));
    if command_is_current && serves {
        HostBundleRegistrationStateV1::Current
    } else {
        HostBundleRegistrationStateV1::Repairable
    }
}

fn antigravity_registration_state(
    component: HostBundleComponentV1,
    home: &Path,
    expected_binary: Option<&str>,
) -> HostBundleRegistrationStateV1 {
    if component != HostBundleComponentV1::ContextMcp {
        return HostBundleRegistrationStateV1::Missing;
    }
    let ide = registration_state(&mcp_config_path(home), expected_binary);
    let cli = registration_state(&cli_plugin_path(home), expected_binary);
    if ide == HostBundleRegistrationStateV1::Corrupt
        || cli == HostBundleRegistrationStateV1::Corrupt
    {
        HostBundleRegistrationStateV1::Corrupt
    } else if ide == HostBundleRegistrationStateV1::Current
        && cli == HostBundleRegistrationStateV1::Current
    {
        HostBundleRegistrationStateV1::Current
    } else if ide == HostBundleRegistrationStateV1::Missing
        && cli == HostBundleRegistrationStateV1::Missing
    {
        HostBundleRegistrationStateV1::Missing
    } else {
        HostBundleRegistrationStateV1::Repairable
    }
}

// ---------------------------------------------------------------------------
// Healthcheck helpers
// ---------------------------------------------------------------------------

fn doctor_check_settings(dc: &mut DoctorCounters, home: &Path) {
    doctor_check_mcp_registration(
        dc,
        &mcp_config_path(home),
        "mcpServers",
        load_json_file,
        &McpDoctorLabels {
            agent_id: "antigravity",
            product: "the Antigravity IDE",
            registered: "IDE MCP server registered",
            missing: "MCP server NOT registered",
        },
    );
}

fn doctor_check_cli_plugin(dc: &mut DoctorCounters, home: &Path) {
    doctor_check_mcp_registration(
        dc,
        &cli_plugin_path(home),
        "mcpServers",
        load_json_file,
        &McpDoctorLabels {
            agent_id: "antigravity",
            product: "the Antigravity CLI (#85)",
            registered: "CLI plugin registered",
            missing: "CLI plugin file exists but lacks `mcpServers.tracedecay`",
        },
    );
}
