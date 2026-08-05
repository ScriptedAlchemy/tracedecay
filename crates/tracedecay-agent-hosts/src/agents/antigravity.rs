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

use std::path::Path;

use super::{
    AgentIntegration, DoctorCounters, HealthcheckContext, McpDoctorLabels,
    doctor_check_mcp_registration, load_json_file,
};

/// Google Antigravity agent.
pub struct AntigravityIntegration;

fn mcp_config_path(home: &Path) -> std::path::PathBuf {
    home.join(".gemini/antigravity/mcp_config.json")
}

/// Per-plugin file used by the Antigravity CLI. Holds the same shape as
/// the IDE config so a future shared loader can read either location.
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

    fn is_detected(&self, home: &Path) -> bool {
        home.join(".gemini/antigravity").is_dir() || home.join(".gemini/antigravity-cli").is_dir()
    }

    fn primary_config_path(&self, home: &Path) -> Option<std::path::PathBuf> {
        Some(mcp_config_path(home))
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        let ide_ok = {
            let mcp_path = mcp_config_path(home);
            if mcp_path.exists() {
                let servers = load_json_file(&mcp_path).get("mcpServers").cloned();
                servers.as_ref().and_then(|v| v.get("tracedecay")).is_some()
            } else {
                false
            }
        };
        let cli_ok = {
            let plugin_path = cli_plugin_path(home);
            let has_entry = |path: &std::path::Path| {
                if !path.exists() {
                    return false;
                }
                let servers = load_json_file(path).get("mcpServers").cloned();
                servers.as_ref().and_then(|v| v.get("tracedecay")).is_some()
            };
            has_entry(&plugin_path)
        };
        ide_ok || cli_ok
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
