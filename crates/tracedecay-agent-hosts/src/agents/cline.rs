//! Cline agent integration.
//!
//! Owns the profile-wide `~/.cline/mcp.json` MCP registration lifecycle.
//!
//! Cline documents this JSON registry as its MCP authority. TraceDecay merges
//! only `mcpServers.tracedecay` and removes only that key, preserving sibling
//! servers and settings. Native Cline hooks remain outside this integration
//! until a real installed-runtime fixture proves their event contract.

use std::path::{Path, PathBuf};

use crate::errors::Result;

use super::{
    AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext, McpDoctorLabels,
    McpUninstallPolicy, config_backup_path, install_mcp_server_entry, load_json_file,
    load_json_file_strict, mcp_registration_entry, mcp_servers_registration_state,
    report_mcp_registration, uninstall_mcp_server_entry,
};

/// Cline agent.
pub struct ClineIntegration;

/// Current Cline CLI/IDE user MCP settings path documented by Cline.
fn cline_mcp_settings_path(home: &Path) -> PathBuf {
    home.join(".cline/mcp.json")
}

/// Legacy VS Code extension storage path retained only for migration diagnosis.
fn legacy_cline_mcp_settings_path(home: &Path) -> PathBuf {
    super::vscode_data_dir(home)
        .join("User/globalStorage/saoudrizwan.claude-dev")
        .join("settings/cline_mcp_settings.json")
}

fn cline_settings_paths(home: &Path) -> [PathBuf; 2] {
    [
        cline_mcp_settings_path(home),
        legacy_cline_mcp_settings_path(home),
    ]
}

/// Cline accepts any `mcpServers.tracedecay` entry, so this deliberately skips
/// the object-shape filter [`super::doctor_check_mcp_registration`] applies.
fn settings_have_tracedecay(path: &Path) -> bool {
    path.exists() && mcp_registration_entry(path, "mcpServers", load_json_file).is_some()
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
        component: super::host_bundle_v2::HostBundleComponentV1,
        ctx: &HealthcheckContext,
    ) -> super::host_bundle_v2::HostBundleRegistrationStateV1 {
        if component != super::host_bundle_v2::HostBundleComponentV1::ContextMcp {
            return super::host_bundle_v2::HostBundleRegistrationStateV1::Missing;
        }
        mcp_servers_registration_state(&cline_mcp_settings_path(&ctx.home))
    }

    fn is_detected(&self, home: &Path) -> bool {
        home.join(".cline").is_dir()
            || legacy_cline_mcp_settings_path(home)
                .parent()
                .is_some_and(Path::is_dir)
    }

    fn primary_config_path(&self, home: &Path) -> Option<PathBuf> {
        Some(cline_mcp_settings_path(home))
    }

    fn host_component_registration_paths(
        &self,
        components: &[super::host_bundle_v2::HostBundleComponentV1],
        home: &Path,
    ) -> Vec<PathBuf> {
        if components == [super::host_bundle_v2::HostBundleComponentV1::ContextMcp] {
            let path = cline_mcp_settings_path(home);
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
                &cline_mcp_settings_path(&ctx.home),
                "mcpServers",
                load_json_file,
                McpUninstallPolicy::default(),
            );
        }
        Ok(())
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        cline_settings_paths(home)
            .iter()
            .any(|path| settings_have_tracedecay(path))
    }
}

// ---------------------------------------------------------------------------
// Healthcheck helpers
// ---------------------------------------------------------------------------

/// Check Cline's `cline_mcp_settings.json` has tracedecay MCP server registered.
///
/// Unlike the plain [`super::doctor_check_mcp_registration`] flow, an absent
/// primary settings file is not a warning on its own: Cline falls through to
/// the legacy VS Code extension path first and only then reports a failure.
fn doctor_check_settings(dc: &mut DoctorCounters, home: &Path) {
    let settings_path = cline_mcp_settings_path(home);
    let registered = settings_have_tracedecay(&settings_path);

    if !registered {
        let legacy_path = legacy_cline_mcp_settings_path(home);
        if settings_have_tracedecay(&legacy_path) {
            dc.warn(&format!(
                "legacy Cline MCP registration found in {} — configure or remove it through Cline's supported flow",
                legacy_path.display()
            ));
            return;
        }
    }

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
