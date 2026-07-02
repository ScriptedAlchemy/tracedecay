// Rust guideline compliant 2025-10-17
//! `OpenCode` agent integration.
//!
//! Handles registration of the tracedecay MCP server in `OpenCode`'s config
//! file (`$HOME/.config/opencode/opencode.json` or `$XDG_CONFIG_HOME/opencode/opencode.json`),
//! and prompt rules via `$HOME/.config/opencode/AGENTS.md`. `OpenCode` has no hook system or
//! declarative tool permissions — it uses interactive runtime approval.

use std::path::Path;

use serde_json::json;

use crate::errors::Result;

use super::{
    backup_and_write_json, backup_config_file, load_json_file, load_json_file_strict,
    safe_write_json_file, AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext,
};

use super::prompt_rules::{PromptRulesOptions, PROMPT_RULE_MARKER};

/// `OpenCode` agent.
pub struct OpenCodeIntegration;

impl AgentIntegration for OpenCodeIntegration {
    fn name(&self) -> &'static str {
        "OpenCode"
    }

    fn id(&self) -> &'static str {
        "opencode"
    }

    fn install(&self, ctx: &InstallContext) -> Result<()> {
        let config_path = opencode_config_path(&ctx.home);
        install_mcp_server(&config_path, &ctx.tracedecay_bin)?;

        let global_prompt = opencode_prompt_path(&ctx.home);
        install_prompt_rules(&global_prompt)?;
        super::install_managed_skill_prompt_index(
            &ctx.home,
            &global_prompt,
            crate::automation::skill_targets::SkillInstallTarget::OpenCode,
        )?;

        eprintln!();
        eprintln!("Setup complete. Next steps:");
        eprintln!("  1. cd into your project and run: tracedecay init");
        eprintln!("  2. Start a new OpenCode session — tracedecay tools are now available");
        eprintln!("  3. OpenCode will prompt for approval on first use of each tool");
        Ok(())
    }

    fn supports_local_install(&self) -> bool {
        true
    }

    fn install_local(&self, ctx: &InstallContext, project_path: &Path) -> Result<()> {
        install_mcp_server(&project_path.join("opencode.json"), &ctx.tracedecay_bin)?;
        let agents_md = project_path.join("AGENTS.md");
        install_prompt_rules(&agents_md)?;
        super::install_managed_skill_prompt_index(
            &ctx.home,
            &agents_md,
            crate::automation::skill_targets::SkillInstallTarget::OpenCode,
        )
    }

    fn uninstall(&self, ctx: &InstallContext) -> Result<()> {
        let config_path = opencode_config_path(&ctx.home);
        uninstall_mcp_server(&config_path);

        let global_prompt = opencode_prompt_path(&ctx.home);
        super::remove_managed_skill_prompt_index(&global_prompt)?;
        uninstall_prompt_rules(&global_prompt);

        eprintln!();
        eprintln!("Uninstall complete. Tracedecay has been removed from OpenCode.");
        eprintln!("Start a new OpenCode session for changes to take effect.");
        Ok(())
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mOpenCode integration\x1b[0m");
        doctor_check_config(dc, &ctx.home);
        doctor_check_prompt(dc, &ctx.home);
    }

    fn is_detected(&self, home: &Path) -> bool {
        home.join(".config").join("opencode").is_dir()
    }

    fn primary_config_path(&self, home: &Path) -> Option<std::path::PathBuf> {
        Some(opencode_config_path(home))
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        let config_path = opencode_config_path(home);
        if !config_path.exists() {
            return false;
        }
        let json = super::load_json_file(&config_path);
        let mcp = json.get("mcp");
        mcp.and_then(|v| v.get("tracedecay")).is_some()
    }
}

// ---------------------------------------------------------------------------
// Config path resolution
// ---------------------------------------------------------------------------

/// Returns the path to opencode config (global).
/// Prefers `$HOME/.config/opencode/opencode.json`. Falls back to
/// `$XDG_CONFIG_HOME/opencode/opencode.json` only when the XDG path
/// is under `home` (so tests with temp-dir homes are never polluted by
/// the real user's environment).
fn opencode_config_path(home: &Path) -> std::path::PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        let xdg_path = std::path::PathBuf::from(&xdg);
        if xdg_path.starts_with(home) {
            return xdg_path.join("opencode/opencode.json");
        }
    }
    home.join(".config/opencode/opencode.json")
}

/// Returns the path to the global AGENTS.md prompt file.
fn opencode_prompt_path(home: &Path) -> std::path::PathBuf {
    let modern = home.join(".config/opencode/AGENTS.md");
    if modern.exists() || home.join(".config/opencode").exists() {
        return modern;
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        let xdg_path = std::path::PathBuf::from(&xdg);
        if xdg_path.starts_with(home) {
            let xdg_dir = xdg_path.join("opencode");
            if xdg_dir.exists() {
                return xdg_dir.join("AGENTS.md");
            }
        }
    }
    home.join("AGENTS.md")
}

// ---------------------------------------------------------------------------
// Install helpers
// ---------------------------------------------------------------------------

/// Register MCP server in opencode.json.
///
/// Safety: creates a `.bak` backup before writing and restores it on any
/// error. Uses strict JSON parsing so an existing file with invalid syntax
/// is never silently replaced with an empty object.
fn install_mcp_server(config_path: &Path, tracedecay_bin: &str) -> Result<()> {
    let backup = backup_config_file(config_path)?;
    let mut config = match load_json_file_strict(config_path) {
        Ok(v) => v,
        Err(e) => {
            if let Some(ref b) = backup {
                eprintln!("  Backup preserved at: {}", b.display());
            }
            return Err(e);
        }
    };

    config["mcp"]["tracedecay"] = json!({
        "type": "local",
        "command": [tracedecay_bin, "serve"]
    });

    safe_write_json_file(config_path, &config, backup.as_deref())?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Added tracedecay MCP server to {}",
        config_path.display()
    );
    Ok(())
}

/// Install-or-refresh prompt rules in AGENTS.md.
fn install_prompt_rules(prompt_path: &Path) -> Result<()> {
    let block = super::prompt_rules::standard_prompt_rules(
        PROMPT_RULE_MARKER,
        &PromptRulesOptions {
            extra_paragraphs: &[],
        },
    );
    super::prompt_rules::reconcile_prompt_rules(prompt_path, PROMPT_RULE_MARKER, &block)
}

// ---------------------------------------------------------------------------
// Uninstall helpers
// ---------------------------------------------------------------------------

/// Remove MCP server from opencode.json.
fn uninstall_mcp_server(config_path: &Path) {
    if !config_path.exists() {
        return;
    }
    let Ok(contents) = std::fs::read_to_string(config_path) else {
        return;
    };
    let Ok(mut config) = serde_json::from_str::<serde_json::Value>(&contents) else {
        return;
    };
    let Some(mcp) = config.get_mut("mcp").and_then(|v| v.as_object_mut()) else {
        return;
    };
    if mcp.remove("tracedecay").is_none() {
        eprintln!(
            "  No tracedecay MCP server in {}, skipping",
            config_path.display()
        );
        return;
    }
    if mcp.is_empty() {
        config.as_object_mut().map(|o| o.remove("mcp"));
    }
    let is_empty = config.as_object().is_some_and(serde_json::Map::is_empty);
    if is_empty {
        std::fs::remove_file(config_path).ok();
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed {} (was empty)",
            config_path.display()
        );
    } else if backup_and_write_json(config_path, &config) {
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed tracedecay MCP server from {}",
            config_path.display()
        );
    }
}

/// Remove tracedecay rules from AGENTS.md.
fn uninstall_prompt_rules(prompt_path: &Path) {
    super::prompt_rules::remove_prompt_rules(prompt_path, PROMPT_RULE_MARKER);
}

// ---------------------------------------------------------------------------
// Healthcheck helpers
// ---------------------------------------------------------------------------

/// Check opencode.json has tracedecay registered.
fn doctor_check_config(dc: &mut DoctorCounters, home: &Path) {
    let config_path = opencode_config_path(home);
    if !config_path.exists() {
        dc.warn(&format!(
            "{} not found — run `tracedecay install --agent opencode` if you use OpenCode",
            config_path.display()
        ));
        return;
    }

    let config = load_json_file(&config_path);
    let mcp_entry = &config["mcp"]["tracedecay"];
    if !mcp_entry.is_object() {
        dc.fail(&format!(
            "MCP server NOT registered in {} — run `tracedecay install --agent opencode`",
            config_path.display()
        ));
        return;
    }
    dc.pass(&format!(
        "MCP server registered in {}",
        config_path.display()
    ));

    let command = mcp_entry["command"].as_array();
    let has_serve = command.is_some_and(|arr| arr.iter().any(|v| v.as_str() == Some("serve")));
    if has_serve {
        dc.pass("MCP server args include \"serve\"");
    } else {
        dc.fail("MCP server args missing \"serve\" — run `tracedecay install --agent opencode`");
    }
}

/// Check AGENTS.md contains tracedecay rules.
fn doctor_check_prompt(dc: &mut DoctorCounters, home: &Path) {
    let prompt_path = opencode_prompt_path(home);
    if prompt_path.exists() {
        let has_rules = std::fs::read_to_string(&prompt_path)
            .unwrap_or_default()
            .contains("tracedecay");
        if has_rules {
            dc.pass("AGENTS.md contains tracedecay rules");
        } else {
            dc.fail(
                "AGENTS.md missing tracedecay rules — run `tracedecay install --agent opencode`",
            );
        }
    } else {
        dc.warn("AGENTS.md does not exist");
    }
}
