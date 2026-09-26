//! Cline agent integration.
//!
//! Owns the profile-wide `~/.cline/mcp.json` MCP registration lifecycle.
//!
//! Cline documents this JSON registry as its MCP authority. TraceDecay merges
//! only `mcpServers.tracedecay` and removes only that key, preserving sibling
//! servers and settings. Native Cline hooks remain outside this integration
//! until a real installed-runtime fixture proves their event contract.

use std::path::{Path, PathBuf};
use tracedecay_runtime_core::config::ProfileRoot;

use tracedecay_domain::errors::Result;

use super::{
    AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext, JsonConfigDialect,
    McpDoctorLabels, McpUninstallPolicy, install_mcp_server_entry, load_json_file,
    mcp_servers_registration_state, report_mcp_registration, uninstall_mcp_server_entry,
};

pub struct ClineIntegration;

/// Current Cline CLI/IDE user MCP settings path documented by Cline.
fn cline_mcp_settings_path(home: &Path) -> PathBuf {
    home.join(".cline/mcp.json")
}

/// Cline accepts any `mcpServers.tracedecay` entry, so this deliberately skips
/// the object-shape filter [`super::doctor_check_mcp_registration`] applies.
fn settings_have_tracedecay(path: &Path) -> bool {
    super::mcp_config_has_tracedecay(path, "mcpServers", load_json_file)
}

impl AgentIntegration for ClineIntegration {
    fn name(&self) -> &'static str {
        "Cline"
    }

    fn id(&self) -> &'static str {
        "cline"
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mCline integration\x1b[0m");
        doctor_check_settings(dc, &ctx.home);
    }

    fn host_component_registration(
        &self,
        component: super::host_bundle::HostComponentV1,
        ctx: &HealthcheckContext,
    ) -> super::host_bundle::HostBundleRegistrationStateV1 {
        if component != super::host_bundle::HostComponentV1::ContextMcp {
            return super::host_bundle::HostBundleRegistrationStateV1::Missing;
        }
        mcp_servers_registration_state(&cline_mcp_settings_path(&ctx.home))
    }

    fn is_detected(&self, home: &Path) -> bool {
        home.join(".cline").is_dir()
    }

    fn primary_config_path(&self, home: &Path, _profile: &ProfileRoot) -> Option<PathBuf> {
        Some(cline_mcp_settings_path(home))
    }

    fn host_component_registration_paths(
        &self,
        components: &[super::host_bundle::HostComponentV1],
        home: &Path,
        _profile: &ProfileRoot,
    ) -> Vec<PathBuf> {
        if components == [super::host_bundle::HostComponentV1::ContextMcp] {
            vec![cline_mcp_settings_path(home)]
        } else {
            Vec::new()
        }
    }

    #[hotpath::measure(label = "cline_mcp_install")]
    fn activate_deployed_host_component_registration(
        &self,
        components: &[super::host_bundle::HostComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        if components.contains(&super::host_bundle::HostComponentV1::ContextMcp) {
            install_mcp_server_entry(
                &cline_mcp_settings_path(&ctx.home),
                "mcpServers",
                serde_json::json!({
                    "command": ctx.tracedecay_bin.clone(),
                    "args": ["serve"],
                    "env": {},
                    "disabled": false,
                    "autoApprove": []
                }),
                "Cline",
                JsonConfigDialect::Json,
            )?;
        }
        Ok(())
    }

    fn deactivate_deployed_host_component_registration(
        &self,
        components: &[super::host_bundle::HostComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        if components.contains(&super::host_bundle::HostComponentV1::ContextMcp) {
            uninstall_mcp_server_entry(
                &cline_mcp_settings_path(&ctx.home),
                "mcpServers",
                JsonConfigDialect::Json,
                McpUninstallPolicy::default(),
            )?;
        }
        Ok(())
    }

    fn has_tracedecay(&self, home: &Path, _profile: &ProfileRoot) -> bool {
        settings_have_tracedecay(&cline_mcp_settings_path(home))
    }
}

// ---------------------------------------------------------------------------
// Healthcheck helpers
// ---------------------------------------------------------------------------

fn doctor_check_settings(dc: &mut DoctorCounters, home: &Path) {
    let settings_path = cline_mcp_settings_path(home);
    let registered = settings_have_tracedecay(&settings_path);
    report_mcp_registration(
        dc,
        &settings_path,
        registered,
        &McpDoctorLabels {
            agent_id: "cline",
            product: "Cline",
            registered: "MCP server registered",
            missing: "MCP server NOT registered",
        },
    );
}
