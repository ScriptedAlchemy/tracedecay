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
    AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext, JsonConfigDialect,
    McpDoctorLabels, McpUninstallPolicy, config_backup_path, doctor_check_mcp_registration,
    install_mcp_server_entry, load_json_file, mcp_servers_registration_state,
    uninstall_mcp_server_entry,
};

pub struct RooCodeIntegration;

fn roo_ext_dir(home: &Path) -> PathBuf {
    super::vscode_data_dir(home).join("User/globalStorage/rooveterinaryinc.roo-cline")
}

fn roo_settings_path(home: &Path) -> PathBuf {
    roo_ext_dir(home).join("settings/cline_mcp_settings.json")
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
        mcp_servers_registration_state(&roo_settings_path(&ctx.home))
    }

    fn is_detected(&self, home: &Path) -> bool {
        roo_ext_dir(home).is_dir()
    }

    fn primary_config_path(&self, home: &Path) -> Option<PathBuf> {
        Some(roo_settings_path(home))
    }

    fn host_component_registration_paths(
        &self,
        components: &[super::host_bundle_v2::HostBundleComponentV1],
        home: &Path,
    ) -> Vec<PathBuf> {
        if components == [super::host_bundle_v2::HostBundleComponentV1::ContextMcp] {
            let path = roo_settings_path(home);
            vec![path.clone(), config_backup_path(&path)]
        } else {
            Vec::new()
        }
    }

    #[hotpath::measure(label = "roo_mcp_install")]
    fn activate_deployed_host_component_registration(
        &self,
        components: &[super::host_bundle_v2::HostBundleComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        if components.contains(&super::host_bundle_v2::HostBundleComponentV1::ContextMcp) {
            install_mcp_server_entry(
                &roo_settings_path(&ctx.home),
                "mcpServers",
                serde_json::json!({
                    "command": ctx.tracedecay_bin.clone(),
                    "args": ["serve"],
                    "env": {},
                    "alwaysAllow": [],
                    "disabled": false
                }),
                "Roo Code",
                JsonConfigDialect::Json,
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
                &roo_settings_path(&ctx.home),
                "mcpServers",
                JsonConfigDialect::Json,
                McpUninstallPolicy::default(),
            )?;
        }
        Ok(())
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        super::mcp_config_has_tracedecay(&roo_settings_path(home), "mcpServers", load_json_file)
    }
}

// ---------------------------------------------------------------------------
// Healthcheck helpers
// ---------------------------------------------------------------------------

fn doctor_check_settings(dc: &mut DoctorCounters, home: &Path) {
    let settings_path = roo_settings_path(home);
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
