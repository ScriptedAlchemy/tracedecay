//! Gemini CLI agent integration.
//!
//! Handles registration of the tracedecay MCP server in Gemini CLI's config
//! file (`~/.gemini/settings.json`), and prompt rules via `~/.gemini/GEMINI.md`.
//! Gemini CLI has no hook system. Tool auto-approval is handled via the
//! `trust: true` flag on the MCP server entry.

use std::path::Path;

use serde_json::json;

use crate::errors::Result;

use super::{
    AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext, McpDoctorLabels,
    McpUninstallPolicy, doctor_check_mcp_registration, install_mcp_server_entry, load_json_file,
    load_json_file_strict, uninstall_mcp_server_entry,
};

use super::prompt_rules::{PROMPT_RULE_MARKER, PromptRulesOptions};

/// Gemini CLI agent.
pub struct GeminiIntegration;

impl AgentIntegration for GeminiIntegration {
    fn name(&self) -> &'static str {
        "Gemini CLI"
    }

    fn id(&self) -> &'static str {
        "gemini"
    }

    fn install(&self, ctx: &InstallContext) -> Result<()> {
        let gemini_dir = ctx.home.join(".gemini");
        std::fs::create_dir_all(&gemini_dir).ok();
        let settings_path = gemini_dir.join("settings.json");

        install_mcp_server(&settings_path, &ctx.tracedecay_bin)?;

        let gemini_md = gemini_dir.join("GEMINI.md");
        install_prompt_rules(&gemini_md)?;

        eprintln!();
        eprintln!("Setup complete. Next steps:");
        eprintln!("  1. cd into your project and run: tracedecay init");
        eprintln!("  2. Start a new Gemini CLI session — tracedecay tools are now available");
        Ok(())
    }

    fn supports_local_install(&self) -> bool {
        true
    }

    fn install_local(&self, ctx: &InstallContext, project_path: &Path) -> Result<()> {
        let settings = project_path.join(".gemini/settings.json");
        let gemini_md = project_path.join("GEMINI.md");
        super::ensure_project_local_safe_paths(
            project_path,
            [settings.as_path(), gemini_md.as_path()],
        )?;
        std::fs::create_dir_all(project_path.join(".gemini")).ok();
        install_mcp_server(&settings, &ctx.tracedecay_bin)?;
        install_prompt_rules(&gemini_md)
    }

    fn uninstall(&self, ctx: &InstallContext) -> Result<()> {
        let gemini_dir = ctx.home.join(".gemini");
        let settings_path = gemini_dir.join("settings.json");

        uninstall_mcp_server(&settings_path);

        let gemini_md = gemini_dir.join("GEMINI.md");
        uninstall_prompt_rules(&gemini_md);

        eprintln!();
        eprintln!("Uninstall complete. Tracedecay has been removed from Gemini CLI.");
        eprintln!("Start a new Gemini CLI session for changes to take effect.");
        Ok(())
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
// Install helpers
// ---------------------------------------------------------------------------

/// Register MCP server in ~/.gemini/settings.json.
fn install_mcp_server(settings_path: &Path, tracedecay_bin: &str) -> Result<()> {
    install_mcp_server_entry(
        settings_path,
        "mcpServers",
        json!({
            "command": tracedecay_bin,
            "args": ["serve"],
            "trust": true
        }),
        "Gemini CLI",
        load_json_file_strict,
    )
}

/// Install-or-refresh prompt rules in GEMINI.md.
fn install_prompt_rules(gemini_md: &Path) -> Result<()> {
    let block = super::prompt_rules::standard_prompt_rules(
        PROMPT_RULE_MARKER,
        &PromptRulesOptions {
            extra_paragraphs: &[],
        },
    );
    super::prompt_rules::reconcile_prompt_rules(gemini_md, PROMPT_RULE_MARKER, &block)
}

// ---------------------------------------------------------------------------
// Uninstall helpers
// ---------------------------------------------------------------------------

/// Remove MCP server from ~/.gemini/settings.json.
fn uninstall_mcp_server(settings_path: &Path) {
    uninstall_mcp_server_entry(
        settings_path,
        "mcpServers",
        load_json_file,
        McpUninstallPolicy {
            prune_empty_root: true,
            remove_empty_file: true,
        },
    );
}

/// Remove tracedecay rules from GEMINI.md.
fn uninstall_prompt_rules(gemini_md: &Path) {
    super::prompt_rules::remove_prompt_rules(gemini_md, PROMPT_RULE_MARKER);
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
