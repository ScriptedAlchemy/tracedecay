//! Roo Code agent integration.
//!
//! Handles registration of the tracedecay MCP server in Roo Code's
//! `cline_mcp_settings.json` under the `mcpServers.tracedecay` key.

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::errors::Result;

use super::{
    AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext, McpDoctorLabels,
    McpUninstallPolicy, doctor_check_mcp_registration, install_mcp_server_entry, load_json_file,
    load_json_file_strict, mcp_servers_registration_state, uninstall_mcp_server_entry,
};

/// Roo Code agent.
pub struct RooCodeIntegration;

/// Returns the Roo Code VS Code extension global storage directory.
fn roo_ext_dir(home: &Path) -> PathBuf {
    super::vscode_data_dir(home).join("User/globalStorage/rooveterinaryinc.roo-cline")
}

impl AgentIntegration for RooCodeIntegration {
    fn name(&self) -> &'static str {
        "Roo Code"
    }

    fn id(&self) -> &'static str {
        "roo-code"
    }

    fn install(&self, ctx: &InstallContext) -> Result<()> {
        let settings_path = roo_ext_dir(&ctx.home).join("settings/cline_mcp_settings.json");
        install_mcp_server(&settings_path, &ctx.tracedecay_bin)?;

        eprintln!();
        eprintln!("Setup complete. Next steps:");
        eprintln!("  1. cd into your project and run: tracedecay init");
        eprintln!("  2. Restart VS Code — tracedecay tools are now available in Roo Code");
        Ok(())
    }

    fn supports_local_install(&self) -> bool {
        true
    }

    fn install_local(&self, ctx: &InstallContext, project_path: &Path) -> Result<()> {
        let mcp_path = project_path.join(".roo/mcp.json");
        super::ensure_project_local_safe_path(project_path, &mcp_path)?;
        install_mcp_server(&mcp_path, &ctx.tracedecay_bin)
    }

    fn uninstall_local(&self, _ctx: &InstallContext, project_path: &Path) -> Result<()> {
        uninstall_mcp_server(&project_path.join(".roo/mcp.json"));
        Ok(())
    }

    fn uninstall(&self, ctx: &InstallContext) -> Result<()> {
        let settings_path = roo_ext_dir(&ctx.home).join("settings/cline_mcp_settings.json");
        uninstall_mcp_server(&settings_path);

        eprintln!();
        eprintln!("Uninstall complete. Tracedecay has been removed from Roo Code.");
        eprintln!("Restart VS Code for changes to take effect.");
        Ok(())
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mRoo Code integration\x1b[0m");
        doctor_check_settings(dc, &ctx.home);
    }

    fn host_component_registration(
        &self,
        _component: super::host_bundle_v2::HostBundleComponentV1,
        ctx: &HealthcheckContext,
    ) -> super::host_bundle_v2::HostBundleRegistrationStateV1 {
        mcp_servers_registration_state(
            &roo_ext_dir(&ctx.home).join("settings/cline_mcp_settings.json"),
        )
    }

    fn is_detected(&self, home: &Path) -> bool {
        roo_ext_dir(home).is_dir()
    }

    fn primary_config_path(&self, home: &Path) -> Option<PathBuf> {
        Some(roo_ext_dir(home).join("settings/cline_mcp_settings.json"))
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        let settings_path = roo_ext_dir(home).join("settings/cline_mcp_settings.json");
        if !settings_path.exists() {
            return false;
        }
        let json = load_json_file(&settings_path);
        let servers = json.get("mcpServers");
        servers.and_then(|v| v.get("tracedecay")).is_some()
    }
}

// ---------------------------------------------------------------------------
// Uninstall helpers
// ---------------------------------------------------------------------------

fn install_mcp_server(settings_path: &Path, tracedecay_bin: &str) -> Result<()> {
    install_mcp_server_entry(
        settings_path,
        "mcpServers",
        json!({
            "command": tracedecay_bin,
            "args": ["serve"],
            "disabled": false
        }),
        "Roo Code",
        load_json_file_strict,
    )
}

/// Remove MCP server entry from Roo Code's `cline_mcp_settings.json`.
fn uninstall_mcp_server(settings_path: &Path) {
    uninstall_mcp_server_entry(
        settings_path,
        "mcpServers",
        load_json_file,
        McpUninstallPolicy {
            prune_empty_root: false,
            remove_empty_file: true,
        },
    );
}

// ---------------------------------------------------------------------------
// Healthcheck helpers
// ---------------------------------------------------------------------------

/// Check Roo Code's `cline_mcp_settings.json` has tracedecay MCP server registered.
fn doctor_check_settings(dc: &mut DoctorCounters, home: &Path) {
    let settings_path = roo_ext_dir(home).join("settings/cline_mcp_settings.json");
    doctor_check_mcp_registration(
        dc,
        &settings_path,
        "mcpServers",
        load_json_file,
        &McpDoctorLabels {
            agent_id: "roo-code",
            product: "Roo Code",
            registered: "MCP server registered",
            missing: "MCP server NOT registered",
        },
    );
}
