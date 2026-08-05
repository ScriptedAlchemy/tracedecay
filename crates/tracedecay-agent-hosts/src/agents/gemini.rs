//! Gemini CLI agent integration.
//!
//! Handles registration of the tracedecay MCP server in Gemini CLI's config
//! file (`~/.gemini/settings.json`), and prompt rules via `~/.gemini/GEMINI.md`.
//! Gemini CLI has no hook system. Tool auto-approval is handled via the
//! `trust: true` flag on the MCP server entry.

use std::path::Path;

use super::{
    AgentIntegration, DoctorCounters, HealthcheckContext, McpDoctorLabels,
    doctor_check_mcp_registration, load_json_file,
};

/// Gemini CLI agent.
pub struct GeminiIntegration;

impl AgentIntegration for GeminiIntegration {
    fn name(&self) -> &'static str {
        "Gemini CLI"
    }

    fn id(&self) -> &'static str {
        "gemini"
    }

    fn supports_local_install(&self) -> bool {
        true
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mGemini CLI integration\x1b[0m");
        doctor_check_settings(dc, &ctx.home);
        doctor_check_prompt(dc, &ctx.home);
    }

    fn is_detected(&self, home: &Path) -> bool {
        home.join(".gemini").is_dir()
    }

    fn primary_config_path(&self, home: &Path) -> Option<std::path::PathBuf> {
        Some(home.join(".gemini/settings.json"))
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        let settings = home.join(".gemini").join("settings.json");
        if !settings.exists() {
            return false;
        }
        let json = super::load_json_file(&settings);
        let servers = json.get("mcpServers");
        servers.and_then(|v| v.get("tracedecay")).is_some()
    }
}

// ---------------------------------------------------------------------------
// Healthcheck helpers
// ---------------------------------------------------------------------------

/// Check settings.json has tracedecay registered.
fn doctor_check_settings(dc: &mut DoctorCounters, home: &Path) {
    let settings_path = home.join(".gemini").join("settings.json");
    let Some(server) = doctor_check_mcp_registration(
        dc,
        &settings_path,
        "mcpServers",
        load_json_file,
        &McpDoctorLabels {
            agent_id: "gemini",
            product: "Gemini CLI",
            registered: "MCP server registered",
            missing: "MCP server NOT registered",
        },
    ) else {
        return;
    };

    // Check command includes "serve"
    let has_serve = server
        .get("args")
        .and_then(|v| v.as_array())
        .is_some_and(|arr| arr.iter().any(|v| v.as_str() == Some("serve")));
    if has_serve {
        dc.pass("MCP server args include \"serve\"");
    } else {
        dc.fail("MCP server args missing \"serve\" — run `tracedecay install --agent gemini`");
    }

    // Check trust flag
    let is_trusted = server
        .get("trust")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if is_trusted {
        dc.pass("MCP server has trust: true (tools auto-approved)");
    } else {
        dc.warn("MCP server missing trust: true — Gemini will prompt for each tool call");
    }
}

/// Check GEMINI.md contains tracedecay rules.
fn doctor_check_prompt(dc: &mut DoctorCounters, home: &Path) {
    let gemini_md = home.join(".gemini").join("GEMINI.md");
    if gemini_md.exists() {
        let has_rules = std::fs::read_to_string(&gemini_md)
            .unwrap_or_default()
            .contains("tracedecay");
        if has_rules {
            dc.pass("GEMINI.md contains tracedecay rules");
        } else {
            dc.fail("GEMINI.md missing tracedecay rules — run `tracedecay install --agent gemini`");
        }
    } else {
        dc.warn("~/.gemini/GEMINI.md does not exist");
    }
}
