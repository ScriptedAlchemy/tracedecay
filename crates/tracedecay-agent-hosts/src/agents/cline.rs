//! Cline agent integration.
//!
//! Handles registration of the tracedecay MCP server in Cline's
//! `cline_mcp_settings.json` under the `mcpServers.tracedecay` key.

use std::env;
use std::path::{Path, PathBuf};

use super::{
    AgentIntegration, DoctorCounters, HealthcheckContext, McpDoctorLabels, load_json_file,
    mcp_registration_entry, mcp_servers_registration_state, report_mcp_registration,
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

    fn supports_local_install(&self) -> bool {
        false
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
