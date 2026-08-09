//! Roo Code agent integration.
//!
//! Owns the profile-wide Roo Code MCP registration lifecycle.
//!
//! Roo documents JSON MCP configuration rather than a non-interactive host
//! command. TraceDecay therefore merges only `mcpServers.tracedecay` in Roo's
//! profile registry and preserves every sibling entry. No native hook route is
//! installed without a real Roo runtime fixture.

use std::path::{Path, PathBuf};

use crate::errors::Result;

use super::{
    AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext, McpDoctorLabels,
    McpUninstallPolicy, config_backup_path, doctor_check_mcp_registration,
    install_mcp_server_entry, load_json_file, load_json_file_strict,
    mcp_servers_registration_state, uninstall_mcp_server_entry,
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

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mRoo Code integration\x1b[0m");
        doctor_check_settings(dc, &ctx.home);
    }

    fn host_component_registration(
        &self,
        component: super::host_bundle_v2::HostBundleComponentV1,
        ctx: &HealthcheckContext,
    ) -> super::host_bundle_v2::HostBundleRegistrationStateV1 {
        if component != super::host_bundle_v2::HostBundleComponentV1::ContextMcp {
            return super::host_bundle_v2::HostBundleRegistrationStateV1::Missing;
        }
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

    fn host_component_registration_paths(
        &self,
        components: &[super::host_bundle_v2::HostBundleComponentV1],
        home: &Path,
    ) -> Vec<PathBuf> {
        if components == [super::host_bundle_v2::HostBundleComponentV1::ContextMcp] {
            let path = roo_ext_dir(home).join("settings/cline_mcp_settings.json");
            vec![path.clone(), config_backup_path(&path)]
        } else {
            Vec::new()
        }
    }

    fn activate_deployed_host_component_registration(
        &self,
        components: &[super::host_bundle_v2::HostBundleComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        if components.contains(&super::host_bundle_v2::HostBundleComponentV1::ContextMcp) {
            install_mcp_server_entry(
                &roo_ext_dir(&ctx.home).join("settings/cline_mcp_settings.json"),
                "mcpServers",
                serde_json::json!({
                    "command": ctx.tracedecay_bin.clone(),
                    "args": ["serve"],
                    "env": {},
                    "alwaysAllow": [],
                    "disabled": false
                }),
                "Roo Code",
                load_json_file_strict,
            )?;
        }
        Ok(())
    }

    fn deactivate_deployed_host_component_registration(
        &self,
        components: &[super::host_bundle_v2::HostBundleComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        if components.contains(&super::host_bundle_v2::HostBundleComponentV1::ContextMcp) {
            uninstall_mcp_server_entry(
                &roo_ext_dir(&ctx.home).join("settings/cline_mcp_settings.json"),
                "mcpServers",
                load_json_file,
                McpUninstallPolicy::default(),
            );
        }
        Ok(())
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
