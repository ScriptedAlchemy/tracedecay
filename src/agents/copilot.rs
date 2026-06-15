//! GitHub Copilot integration.
//!
//! Handles registration of the tracedecay MCP server in both:
//! - VS Code's `settings.json` under `mcp.servers.tracedecay`
//! - Copilot CLI's `~/.copilot/mcp-config.json` under `mcpServers.tracedecay`

use std::path::Path;

use serde_json::json;

use crate::errors::Result;

use super::{
    backup_and_write_json, backup_config_file, load_json_file, load_json_file_strict,
    load_jsonc_file, load_jsonc_file_strict, safe_write_json_file, AgentIntegration,
    DoctorCounters, HealthcheckContext, InstallContext,
};

const PROMPT_RULE_MARKER: &str = "## Prefer tracedecay MCP tools";
const LEGACY_PROMPT_RULE_MARKER: &str = "## Prefer tokensave MCP tools";

/// GitHub Copilot agent.
pub struct CopilotIntegration;

impl AgentIntegration for CopilotIntegration {
    fn name(&self) -> &'static str {
        "GitHub Copilot"
    }

    fn id(&self) -> &'static str {
        "copilot"
    }

    fn install(&self, ctx: &InstallContext) -> Result<()> {
        let vscode_settings_path = super::vscode_data_dir(&ctx.home).join("User/settings.json");
        let cli_settings_path = super::copilot_cli_dir(&ctx.home).join("mcp-config.json");

        install_vscode_mcp_server(&vscode_settings_path, &ctx.tracedecay_bin)?;
        let insiders_settings_path =
            super::vscode_insiders_data_dir(&ctx.home).join("User/settings.json");
        if insiders_settings_path
            .parent()
            .is_some_and(std::path::Path::exists)
        {
            install_vscode_mcp_server(&insiders_settings_path, &ctx.tracedecay_bin)?;
        }
        install_cli_mcp_server(&cli_settings_path, &ctx.tracedecay_bin)?;

        // Install prompt rules
        let vscode_instructions =
            super::vscode_data_dir(&ctx.home).join("User/prompts/copilot-instructions.md");
        install_prompt_rules(&vscode_instructions)?;
        let insiders_instructions =
            super::vscode_insiders_data_dir(&ctx.home).join("User/prompts/copilot-instructions.md");
        if super::vscode_insiders_data_dir(&ctx.home)
            .join("User")
            .exists()
        {
            install_prompt_rules(&insiders_instructions)?;
        }
        let cli_instructions = super::copilot_cli_dir(&ctx.home).join("copilot-instructions.md");
        install_prompt_rules(&cli_instructions)?;

        eprintln!();
        eprintln!("Setup complete. Next steps:");
        eprintln!("  1. cd into your project and run: tracedecay init");
        eprintln!("  2. Restart VS Code and/or start a new Copilot CLI session");
        eprintln!("     tracedecay tools are now available in GitHub Copilot");
        Ok(())
    }

    fn supports_local_install(&self) -> bool {
        true
    }

    fn install_local(&self, ctx: &InstallContext, project_path: &Path) -> Result<()> {
        install_workspace_mcp_server(&project_path.join(".vscode/mcp.json"), &ctx.tracedecay_bin)?;
        install_prompt_rules(&project_path.join(".github/copilot-instructions.md"))
    }

    fn uninstall(&self, ctx: &InstallContext) -> Result<()> {
        let vscode_settings_path = super::vscode_data_dir(&ctx.home).join("User/settings.json");
        let cli_settings_path = super::copilot_cli_dir(&ctx.home).join("mcp-config.json");
        uninstall_vscode_mcp_server(&vscode_settings_path);
        let insiders_settings_path =
            super::vscode_insiders_data_dir(&ctx.home).join("User/settings.json");
        uninstall_vscode_mcp_server(&insiders_settings_path);
        uninstall_cli_mcp_server(&cli_settings_path);

        let vscode_instructions =
            super::vscode_data_dir(&ctx.home).join("User/prompts/copilot-instructions.md");
        uninstall_prompt_rules(&vscode_instructions);
        let insiders_instructions =
            super::vscode_insiders_data_dir(&ctx.home).join("User/prompts/copilot-instructions.md");
        uninstall_prompt_rules(&insiders_instructions);
        let cli_instructions = super::copilot_cli_dir(&ctx.home).join("copilot-instructions.md");
        uninstall_prompt_rules(&cli_instructions);

        eprintln!();
        eprintln!("Uninstall complete. Tracedecay has been removed from GitHub Copilot.");
        eprintln!(
            "Restart VS Code and/or start a new Copilot CLI session for changes to take effect."
        );
        Ok(())
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mGitHub Copilot integration\x1b[0m");
        doctor_check_vscode_settings(dc, &super::vscode_data_dir(&ctx.home), "VS Code");
        doctor_check_vscode_settings(
            dc,
            &super::vscode_insiders_data_dir(&ctx.home),
            "VS Code Insiders",
        );
        doctor_check_cli_settings(dc, &ctx.home);
    }

    fn is_detected(&self, home: &Path) -> bool {
        super::vscode_data_dir(home).join("User").is_dir()
            || super::vscode_insiders_data_dir(home).join("User").is_dir()
            || super::copilot_cli_dir(home).is_dir()
    }

    fn primary_config_path(&self, home: &Path) -> Option<std::path::PathBuf> {
        Some(super::vscode_data_dir(home).join("User/settings.json"))
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        let vscode_settings_path = super::vscode_data_dir(home).join("User/settings.json");
        let insiders_settings_path =
            super::vscode_insiders_data_dir(home).join("User/settings.json");
        let cli_settings_path = super::copilot_cli_dir(home).join("mcp-config.json");

        let vscode_has_tracedecay = if vscode_settings_path.exists() {
            let json = load_jsonc_file(&vscode_settings_path);
            let servers = json.get("mcp").and_then(|v| v.get("servers"));
            servers.and_then(|v| v.get("tracedecay")).is_some()
                || servers.and_then(|v| v.get("tokensave")).is_some()
        } else {
            false
        };

        let insiders_has_tracedecay = if insiders_settings_path.exists() {
            let json = load_jsonc_file(&insiders_settings_path);
            let servers = json.get("mcp").and_then(|v| v.get("servers"));
            servers.and_then(|v| v.get("tracedecay")).is_some()
                || servers.and_then(|v| v.get("tokensave")).is_some()
        } else {
            false
        };

        let cli_has_tracedecay = if cli_settings_path.exists() {
            let json = load_json_file(&cli_settings_path);
            let servers = json.get("mcpServers");
            servers.and_then(|v| v.get("tracedecay")).is_some()
                || servers.and_then(|v| v.get("tokensave")).is_some()
        } else {
            false
        };

        vscode_has_tracedecay || insiders_has_tracedecay || cli_has_tracedecay
    }
}

/// Register MCP server in VS Code settings.json.
fn install_vscode_mcp_server(settings_path: &Path, tracedecay_bin: &str) -> Result<()> {
    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let backup = backup_config_file(settings_path)?;
    let mut settings = match load_jsonc_file_strict(settings_path) {
        Ok(v) => v,
        Err(e) => {
            if let Some(ref b) = backup {
                eprintln!("  Backup preserved at: {}", b.display());
            }
            return Err(e);
        }
    };
    settings["mcp"]["servers"]["tracedecay"] = json!({
        "type": "stdio",
        "command": tracedecay_bin,
        "args": ["serve"]
    });

    safe_write_json_file(settings_path, &settings, backup.as_deref())?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Added tracedecay MCP server to {}",
        settings_path.display()
    );
    Ok(())
}

/// Register MCP server in Copilot CLI's ~/.copilot/mcp-config.json.
fn install_cli_mcp_server(settings_path: &Path, tracedecay_bin: &str) -> Result<()> {
    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let backup = backup_config_file(settings_path)?;
    let mut settings = match load_json_file_strict(settings_path) {
        Ok(v) => v,
        Err(e) => {
            if let Some(ref b) = backup {
                eprintln!("  Backup preserved at: {}", b.display());
            }
            return Err(e);
        }
    };
    settings["mcpServers"]["tracedecay"] = json!({
        "type": "stdio",
        "command": tracedecay_bin,
        "args": ["serve"]
    });

    safe_write_json_file(settings_path, &settings, backup.as_deref())?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Added tracedecay MCP server to {}",
        settings_path.display()
    );
    Ok(())
}

/// Register MCP server in a project `.vscode/mcp.json` file.
fn install_workspace_mcp_server(settings_path: &Path, tracedecay_bin: &str) -> Result<()> {
    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let backup = backup_config_file(settings_path)?;
    let mut settings = match load_jsonc_file_strict(settings_path) {
        Ok(v) => v,
        Err(e) => {
            if let Some(ref b) = backup {
                eprintln!("  Backup preserved at: {}", b.display());
            }
            return Err(e);
        }
    };
    settings["servers"]["tracedecay"] = json!({
        "type": "stdio",
        "command": tracedecay_bin,
        "args": ["serve"]
    });

    safe_write_json_file(settings_path, &settings, backup.as_deref())?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Added tracedecay MCP server to {}",
        settings_path.display()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Uninstall helpers
// ---------------------------------------------------------------------------

/// Remove MCP server entry from VS Code settings.json.
/// Does not delete the file even if the object becomes empty (other VS Code
/// settings may still exist).
fn uninstall_vscode_mcp_server(settings_path: &Path) {
    if !settings_path.exists() {
        eprintln!("  {} not found, skipping", settings_path.display());
        return;
    }

    let mut settings = load_jsonc_file(settings_path);

    // Remove both new and legacy server keys.
    let removed = if let Some(map) = settings
        .get_mut("mcp")
        .and_then(|mcp| mcp.get_mut("servers"))
        .and_then(|servers| servers.as_object_mut())
    {
        let removed_new = map.remove("tracedecay").is_some();
        let removed_legacy = map.remove("tokensave").is_some();
        removed_new || removed_legacy
    } else {
        false
    };

    if !removed {
        eprintln!(
            "  No tracedecay/tokensave MCP server in {}, skipping",
            settings_path.display()
        );
        return;
    }

    // Clean up empty "servers" object
    if let Some(mcp) = settings.get_mut("mcp") {
        let servers_empty = mcp
            .get("servers")
            .and_then(|v| v.as_object())
            .is_some_and(serde_json::Map::is_empty);
        if servers_empty {
            mcp.as_object_mut().map(|o| o.remove("servers"));
        }

        // Clean up empty "mcp" object
        let mcp_empty = settings
            .get("mcp")
            .and_then(|v| v.as_object())
            .is_some_and(serde_json::Map::is_empty);
        if mcp_empty {
            settings.as_object_mut().map(|o| o.remove("mcp"));
        }
    }

    // Always write back (never delete settings.json — it has other VS Code settings).
    // backup_and_write_json leaves a .bak so any mistake is recoverable (issue #63).
    if backup_and_write_json(settings_path, &settings) {
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed tracedecay/tokensave MCP server from {}",
            settings_path.display()
        );
    }
}

/// Remove MCP server entry from Copilot CLI's ~/.copilot/mcp-config.json.
fn uninstall_cli_mcp_server(settings_path: &Path) {
    if !settings_path.exists() {
        return;
    }
    let Ok(contents) = std::fs::read_to_string(settings_path) else {
        return;
    };
    let Ok(mut settings) = serde_json::from_str::<serde_json::Value>(&contents) else {
        return;
    };
    let Some(servers) = settings
        .get_mut("mcpServers")
        .and_then(|v| v.as_object_mut())
    else {
        return;
    };
    let removed_new = servers.remove("tracedecay").is_some();
    let removed_legacy = servers.remove("tokensave").is_some();
    if !removed_new && !removed_legacy {
        eprintln!(
            "  No tracedecay/tokensave MCP server in {}, skipping",
            settings_path.display()
        );
        return;
    }
    if servers.is_empty() {
        settings.as_object_mut().map(|o| o.remove("mcpServers"));
    }
    let is_empty = settings.as_object().is_some_and(serde_json::Map::is_empty);
    if is_empty {
        std::fs::remove_file(settings_path).ok();
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed {} (was empty)",
            settings_path.display()
        );
    } else if backup_and_write_json(settings_path, &settings) {
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed tracedecay/tokensave MCP server from {}",
            settings_path.display()
        );
    }
}

// ---------------------------------------------------------------------------
// Prompt rules helpers
// ---------------------------------------------------------------------------

/// Append prompt rules to a copilot-instructions.md file (idempotent).
fn install_prompt_rules(instructions_path: &Path) -> Result<()> {
    use std::io::Write;
    let existing = if instructions_path.exists() {
        std::fs::read_to_string(instructions_path).unwrap_or_default()
    } else {
        String::new()
    };
    if existing.contains(PROMPT_RULE_MARKER) {
        eprintln!(
            "  {} already contains tracedecay rules, skipping",
            instructions_path.display()
        );
        return Ok(());
    }
    if let Some(parent) = instructions_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(instructions_path)
        .map_err(|e| crate::errors::TraceDecayError::Config {
            message: format!("failed to open {}: {e}", instructions_path.display()),
        })?;
    write!(
        f,
        "\n{PROMPT_RULE_MARKER}\n\n\
        Before reading source files or scanning the codebase, use the tracedecay MCP tools \
        (`tracedecay_context`, `tracedecay_search`, `tracedecay_callers`, `tracedecay_callees`, \
        `tracedecay_impact`, `tracedecay_node`, `tracedecay_files`, `tracedecay_affected`). \
        They provide instant semantic results from a pre-built knowledge graph and are \
        faster than file reads.\n\n\
        If a code analysis question cannot be fully answered by tracedecay MCP tools, \
        try querying the SQLite database directly at `.tracedecay/tracedecay.db` \
        (tables: `nodes`, `edges`, `files`, `memory_facts`, `memory_entities`, \
        `memory_feedback_events`). Use SQL to answer complex structural queries \
        that go beyond what the built-in tools expose.\n\n\
        For durable project/user facts, prefer `tracedecay_fact_store`, \
        `tracedecay_fact_feedback`, and `tracedecay_memory_status` over ad-hoc notes. \
        Use `tracedecay_message_search` for project-local Cursor transcript recall when \
        prior conversation context matters. Do not store secrets, credentials, or \
        unnecessary PII in persistent facts.\n\n\
        If you find a gap where tracedecay could answer a question natively, propose opening \
        an issue at https://github.com/ScriptedAlchemy/tracedecay. Remind the user to strip \
        sensitive or proprietary code from any issue text before submitting.\n"
    )
    .map_err(|e| crate::errors::TraceDecayError::Config {
        message: format!("failed to write {}: {e}", instructions_path.display()),
    })?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Added tracedecay rules to {}",
        instructions_path.display()
    );
    Ok(())
}

/// Remove tracedecay rules from a copilot-instructions.md file.
fn uninstall_prompt_rules(instructions_path: &Path) {
    if !instructions_path.exists() {
        return;
    }
    let Ok(contents) = std::fs::read_to_string(instructions_path) else {
        return;
    };
    if !contents.contains("tracedecay") && !contents.contains("tokensave") {
        return;
    }
    let marker = if contents.contains(PROMPT_RULE_MARKER) {
        PROMPT_RULE_MARKER
    } else {
        LEGACY_PROMPT_RULE_MARKER
    };
    let Some(start) = contents.find(marker) else {
        return;
    };
    let after_marker = start + marker.len();
    let end = contents[after_marker..]
        .find("\n## ")
        .map_or(contents.len(), |pos| after_marker + pos);
    let mut new_contents = String::new();
    new_contents.push_str(contents[..start].trim_end());
    let remainder = &contents[end..];
    if !remainder.is_empty() {
        new_contents.push_str("\n\n");
        new_contents.push_str(remainder.trim_start());
    }
    let new_contents = new_contents.trim().to_string();
    if new_contents.is_empty() {
        std::fs::remove_file(instructions_path).ok();
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed {} (was empty)",
            instructions_path.display()
        );
    } else {
        std::fs::write(instructions_path, format!("{new_contents}\n")).ok();
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed tracedecay rules from {}",
            instructions_path.display()
        );
    }
}

// ---------------------------------------------------------------------------
// Healthcheck helpers
// ---------------------------------------------------------------------------

/// Check VS Code (or VS Code Insiders) settings.json has tracedecay MCP server registered.
fn doctor_check_vscode_settings(dc: &mut DoctorCounters, vscode_dir: &Path, label: &str) {
    let settings_path = vscode_dir.join("User/settings.json");

    if !settings_path.exists() {
        dc.warn(&format!(
            "{} not found — run `tracedecay install --agent copilot` if you use GitHub Copilot in {label}",
            settings_path.display()
        ));
        return;
    }

    let settings = load_jsonc_file(&settings_path);
    let server = settings
        .get("mcp")
        .and_then(|v| v.get("servers"))
        .and_then(|v| v.get("tracedecay"));

    let Some(server) = server.and_then(|v| v.as_object()) else {
        dc.fail(&format!(
            "MCP server NOT registered in {} — run `tracedecay install --agent copilot`",
            settings_path.display()
        ));
        return;
    };
    dc.pass(&format!(
        "MCP server registered in {}",
        settings_path.display()
    ));

    // Check args include "serve"
    let has_serve = server
        .get("args")
        .and_then(|v| v.as_array())
        .is_some_and(|arr| arr.iter().any(|v| v.as_str() == Some("serve")));
    if has_serve {
        dc.pass("MCP server args include \"serve\"");
    } else {
        dc.fail("MCP server args missing \"serve\" — run `tracedecay install --agent copilot`");
    }
}

/// Check Copilot CLI mcp-config.json has tracedecay MCP server registered.
fn doctor_check_cli_settings(dc: &mut DoctorCounters, home: &Path) {
    let settings_path = super::copilot_cli_dir(home).join("mcp-config.json");

    if !settings_path.exists() {
        dc.warn(&format!(
            "{} not found — run `tracedecay install --agent copilot` if you use Copilot CLI",
            settings_path.display()
        ));
        return;
    }

    let settings = load_json_file(&settings_path);
    let server = settings.get("mcpServers").and_then(|v| v.get("tracedecay"));

    let Some(server) = server.and_then(|v| v.as_object()) else {
        dc.fail(&format!(
            "MCP server NOT registered in {} — run `tracedecay install --agent copilot`",
            settings_path.display()
        ));
        return;
    };
    dc.pass(&format!(
        "MCP server registered in {}",
        settings_path.display()
    ));

    let has_serve = server
        .get("args")
        .and_then(|v| v.as_array())
        .is_some_and(|arr| arr.iter().any(|v| v.as_str() == Some("serve")));
    if has_serve {
        dc.pass("MCP server args include \"serve\"");
    } else {
        dc.fail("MCP server args missing \"serve\" — run `tracedecay install --agent copilot`");
    }
}
