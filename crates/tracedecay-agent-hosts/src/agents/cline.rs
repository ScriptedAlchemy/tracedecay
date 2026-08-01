//! Cline agent integration.
//!
//! Handles registration of the tracedecay MCP server in Cline's
//! `cline_mcp_settings.json` under the `mcpServers.tracedecay` key.

use std::env;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::errors::{Result, TraceDecayError};

use super::{
    AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext, McpDoctorLabels,
    McpUninstallPolicy, install_mcp_server_entry, load_json_file, load_json_file_strict,
    mcp_registration_entry, mcp_servers_registration_state, report_mcp_registration,
    uninstall_mcp_server_entry,
};

/// Cline agent.
pub struct ClineIntegration;

fn cline_data_dir(home: &Path) -> PathBuf {
    env::var_os("CLINE_DATA_DIR")
        .filter(|value| !value.is_empty())
        .map_or_else(|| home.join(".cline/data"), PathBuf::from)
}

/// Current Cline CLI/IDE user MCP settings path.
fn cline_mcp_settings_path(home: &Path) -> PathBuf {
    cline_data_dir(home).join("settings/cline_mcp_settings.json")
}

/// Legacy VS Code extension storage path retained only for migration/removal.
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

    fn install(&self, ctx: &InstallContext) -> Result<()> {
        let settings_path = cline_mcp_settings_path(&ctx.home);
        install_mcp_server(&settings_path, &ctx.tracedecay_bin)?;
        let legacy_path = legacy_cline_mcp_settings_path(&ctx.home);
        if legacy_path != settings_path && settings_have_tracedecay(&legacy_path) {
            uninstall_mcp_server(&legacy_path);
            eprintln!(
                "\x1b[32m✔\x1b[0m Removed legacy duplicate registration from {}",
                legacy_path.display()
            );
        }

        eprintln!();
        eprintln!("Setup complete. Next steps:");
        eprintln!("  1. cd into your project and run: tracedecay init");
        eprintln!("  2. Restart Cline — tracedecay tools are now available");
        Ok(())
    }

    fn supports_local_install(&self) -> bool {
        false
    }

    fn install_local(&self, _ctx: &InstallContext, _project_path: &Path) -> Result<()> {
        Err(TraceDecayError::Config {
            message: "Cline does not currently document or ship a project-local MCP config path. \
                      `tracedecay install --local --agent cline` is unsupported. \
                      Run `tracedecay install --agent cline` for a global install."
                .to_string(),
        })
    }

    fn uninstall(&self, ctx: &InstallContext) -> Result<()> {
        for settings_path in cline_settings_paths(&ctx.home) {
            uninstall_mcp_server(&settings_path);
        }

        eprintln!();
        eprintln!("Uninstall complete. Tracedecay has been removed from Cline.");
        eprintln!("Restart VS Code for changes to take effect.");
        Ok(())
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mCline integration\x1b[0m");
        doctor_check_settings(dc, &ctx.home);
    }

    fn host_component_registration(
        &self,
        _component: super::host_bundle_v2::HostBundleComponentV1,
        ctx: &HealthcheckContext,
    ) -> super::host_bundle_v2::HostBundleRegistrationStateV1 {
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

    fn has_tracedecay(&self, home: &Path) -> bool {
        cline_settings_paths(home)
            .iter()
            .any(|path| settings_have_tracedecay(path))
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
        "Cline",
        load_json_file_strict,
    )
}

/// Remove MCP server entry from Cline's `cline_mcp_settings.json`.
fn uninstall_mcp_server(settings_path: &Path) {
    uninstall_mcp_server_entry(
        settings_path,
        "mcpServers",
        load_json_file,
        McpUninstallPolicy::default(),
    );
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
                "legacy Cline MCP registration found in {} — run `tracedecay install --agent cline` to repair",
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
