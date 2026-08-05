//! Roo Code agent integration.
//!
//! Handles registration of the tracedecay MCP server in Roo Code's
//! `cline_mcp_settings.json` under the `mcpServers.tracedecay` key.

use std::path::{Path, PathBuf};

use super::{
    AgentIntegration, DoctorCounters, HealthcheckContext, McpDoctorLabels,
    doctor_check_mcp_registration, load_json_file, mcp_servers_registration_state,
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

    fn supports_local_install(&self) -> bool {
        true
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
