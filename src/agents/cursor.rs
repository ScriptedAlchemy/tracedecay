//! Cursor agent integration.
//!
//! Handles registration of the tokensave MCP server in Cursor's
//! `~/.cursor/mcp.json` under the `mcpServers.tokensave` key.

use std::path::Path;

use serde_json::json;

use crate::errors::Result;

use super::{
    backup_and_write_json, backup_config_file, load_json_file, load_json_file_strict,
    load_jsonc_file_strict, read_only_tool_names, safe_write_json_file, safe_write_text_file,
    AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext,
};

/// Cursor agent.
pub struct CursorIntegration;

impl AgentIntegration for CursorIntegration {
    fn name(&self) -> &'static str {
        "Cursor"
    }

    fn id(&self) -> &'static str {
        "cursor"
    }

    fn install(&self, ctx: &InstallContext) -> Result<()> {
        install_mcp_server(&ctx.home.join(".cursor/mcp.json"), &ctx.tokensave_bin)?;

        eprintln!();
        eprintln!("Setup complete. Next steps:");
        eprintln!("  1. cd into your project and run: tokensave init");
        eprintln!("  2. Restart Cursor — tokensave tools are now available");
        Ok(())
    }

    fn supports_local_install(&self) -> bool {
        true
    }

    fn install_local(&self, ctx: &InstallContext, project_path: &Path) -> Result<()> {
        let cursor_dir = project_path.join(".cursor");
        let mcp_path = cursor_dir.join("mcp.json");
        let rule_path = cursor_dir.join("rules/tokensave.mdc");
        let permissions_path = cursor_dir.join("permissions.json");
        let hooks_path = cursor_dir.join("hooks.json");
        for path in [&mcp_path, &rule_path, &permissions_path, &hooks_path] {
            super::ensure_project_local_safe_path(project_path, path)?;
        }
        install_mcp_server(&mcp_path, &ctx.tokensave_bin)?;
        install_project_rule(&rule_path)?;
        install_permissions(&permissions_path)?;
        install_hooks(&hooks_path, &ctx.tokensave_bin)
    }

    fn uninstall(&self, ctx: &InstallContext) -> Result<()> {
        let mcp_path = ctx.home.join(".cursor/mcp.json");
        uninstall_mcp_server(&mcp_path);

        eprintln!();
        eprintln!("Uninstall complete. Tokensave has been removed from Cursor.");
        eprintln!("Restart Cursor for changes to take effect.");
        Ok(())
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mCursor integration\x1b[0m");
        doctor_check_settings(dc, &ctx.home);
    }

    fn is_detected(&self, home: &Path) -> bool {
        home.join(".cursor").is_dir()
    }

    fn primary_config_path(&self, home: &Path) -> Option<std::path::PathBuf> {
        Some(home.join(".cursor/mcp.json"))
    }

    fn has_tokensave(&self, home: &Path) -> bool {
        let mcp_path = home.join(".cursor/mcp.json");
        if !mcp_path.exists() {
            return false;
        }
        let json = load_json_file(&mcp_path);
        json.get("mcpServers")
            .and_then(|v| v.get("tokensave"))
            .is_some()
    }
}

// ---------------------------------------------------------------------------
// Uninstall helpers
// ---------------------------------------------------------------------------

fn install_mcp_server(mcp_path: &Path, tokensave_bin: &str) -> Result<()> {
    if let Some(parent) = mcp_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let backup = backup_config_file(mcp_path)?;
    let mut settings = match load_json_file_strict(mcp_path) {
        Ok(v) => v,
        Err(e) => {
            if let Some(ref b) = backup {
                eprintln!("  Backup preserved at: {}", b.display());
            }
            return Err(e);
        }
    };
    settings["mcpServers"]["tokensave"] = json!({
        "type": "stdio",
        "command": tokensave_bin,
        "args": ["serve"]
    });

    safe_write_json_file(mcp_path, &settings, backup.as_deref())?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Added tokensave MCP server to {}",
        mcp_path.display()
    );
    Ok(())
}

fn install_project_rule(rule_path: &Path) -> Result<()> {
    let contents = r#"---
description: Prefer tokensave MCP tools for codebase exploration
alwaysApply: true
---

# Prefer tokensave MCP tools

- For codebase exploration, symbol lookup, call graphs, callers/callees, impact analysis, affected files, and architectural navigation, use the tokensave MCP tools first.
- Prefer tools such as `tokensave_context`, `tokensave_search`, `tokensave_callers`, `tokensave_callees`, `tokensave_impact`, `tokensave_files`, `tokensave_affected`, and related read-only tokensave tools before broad file reads or search.
- Only fall back to regular file reads, search, or shell commands when tokensave cannot answer the question or after tokensave has identified the exact files or symbols to inspect.
"#;
    write_generated_text(rule_path, contents)?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Wrote Cursor project rule to {}",
        rule_path.display()
    );
    Ok(())
}

fn install_permissions(permissions_path: &Path) -> Result<()> {
    let backup = backup_config_file(permissions_path)?;
    let mut permissions = match load_jsonc_file_strict(permissions_path) {
        Ok(v) => v,
        Err(e) => {
            if let Some(ref b) = backup {
                eprintln!("  Backup preserved at: {}", b.display());
            }
            return Err(e);
        }
    };

    let existing = permissions["mcpAllowlist"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut allow = existing;
    for tool in read_only_tool_names() {
        let entry = format!("tokensave:{tool}");
        if !allow.iter().any(|existing| existing == &entry) {
            allow.push(entry);
        }
    }
    permissions["mcpAllowlist"] = json!(allow);

    safe_write_json_file(permissions_path, &permissions, backup.as_deref())?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Added Cursor MCP permissions to {}",
        permissions_path.display()
    );
    Ok(())
}

fn install_hooks(hooks_path: &Path, tokensave_bin: &str) -> Result<()> {
    let backup = backup_config_file(hooks_path)?;
    let mut hooks = match load_jsonc_file_strict(hooks_path) {
        Ok(v) => v,
        Err(e) => {
            if let Some(ref b) = backup {
                eprintln!("  Backup preserved at: {}", b.display());
            }
            return Err(e);
        }
    };

    hooks["version"] = json!(1);
    // Reduce wasteful exploration.
    install_cursor_hook_entry(
        &mut hooks,
        "sessionStart",
        tokensave_bin,
        "hook-cursor-session-start",
        5,
        None,
    );
    install_cursor_hook_entry(
        &mut hooks,
        "subagentStart",
        tokensave_bin,
        "hook-cursor-subagent-start",
        5,
        None,
    );
    install_cursor_hook_entry(
        &mut hooks,
        "preToolUse",
        tokensave_bin,
        "hook-cursor-pre-tool-use",
        5,
        Some("Shell|Bash|Read|ReadFile|Grep|Glob|Search|Task"),
    );
    install_cursor_hook_entry(
        &mut hooks,
        "beforeSubmitPrompt",
        tokensave_bin,
        "hook-cursor-before-submit-prompt",
        5,
        None,
    );
    // Keep the index fresh. afterFileEdit uses a targeted single-file sync and
    // is scoped to agent `Write` edits via a matcher.
    install_cursor_hook_entry(
        &mut hooks,
        "afterFileEdit",
        tokensave_bin,
        "hook-cursor-after-file-edit",
        30,
        Some("Write"),
    );
    install_cursor_hook_entry(
        &mut hooks,
        "afterShellExecution",
        tokensave_bin,
        "hook-cursor-after-shell",
        60,
        None,
    );
    install_cursor_hook_entry(
        &mut hooks,
        "workspaceOpen",
        tokensave_bin,
        "hook-cursor-workspace-open",
        60,
        None,
    );

    safe_write_json_file(hooks_path, &hooks, backup.as_deref())?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Added Cursor project hooks to {}",
        hooks_path.display()
    );
    Ok(())
}

fn install_cursor_hook_entry(
    hooks: &mut serde_json::Value,
    event: &str,
    tokensave_bin: &str,
    subcommand: &str,
    timeout: u64,
    matcher: Option<&str>,
) {
    let existing = hooks["hooks"][event]
        .as_array()
        .cloned()
        .unwrap_or_default();

    // Rebuild the tokensave-owned entry every install so refinements (matcher,
    // timeout) reach pre-existing configs, while preserving any foreign hooks.
    // Idempotent: there is always exactly one tokensave entry per event.
    let mut event_hooks: Vec<serde_json::Value> = existing
        .into_iter()
        .filter(|hook| {
            !hook
                .get("command")
                .and_then(|v| v.as_str())
                .is_some_and(|command| command.contains(subcommand))
        })
        .collect();

    let mut entry = json!({
        "command": super::hook_command(tokensave_bin, subcommand),
        "timeout": timeout
    });
    if let Some(matcher) = matcher {
        entry["matcher"] = json!(matcher);
    }
    event_hooks.push(entry);

    hooks["hooks"][event] = serde_json::Value::Array(event_hooks);
}

fn write_generated_text(path: &Path, contents: &str) -> Result<()> {
    let backup = if path.exists() {
        let existing = std::fs::read(path).map_err(|e| crate::errors::TokenSaveError::Config {
            message: format!("failed to read {} before writing: {e}", path.display()),
        })?;
        if existing == contents.as_bytes() {
            return Ok(());
        }
        backup_config_file(path)?
    } else {
        None
    };
    safe_write_text_file(path, contents, backup.as_deref())
}

/// Remove MCP server entry from ~/.cursor/mcp.json.
fn uninstall_mcp_server(mcp_path: &Path) {
    if !mcp_path.exists() {
        eprintln!("  {} not found, skipping", mcp_path.display());
        return;
    }

    let Ok(contents) = std::fs::read_to_string(mcp_path) else {
        return;
    };
    let Ok(mut settings) = serde_json::from_str::<serde_json::Value>(&contents) else {
        return;
    };

    let Some(servers) = settings
        .get_mut("mcpServers")
        .and_then(|v| v.as_object_mut())
    else {
        eprintln!(
            "  No tokensave MCP server in {}, skipping",
            mcp_path.display()
        );
        return;
    };

    if servers.remove("tokensave").is_none() {
        eprintln!(
            "  No tokensave MCP server in {}, skipping",
            mcp_path.display()
        );
        return;
    }

    let is_empty = settings.as_object().is_some_and(|o| {
        o.iter()
            .all(|(k, v)| k == "mcpServers" && v.as_object().is_some_and(serde_json::Map::is_empty))
    });

    if is_empty {
        std::fs::remove_file(mcp_path).ok();
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed {} (was empty)",
            mcp_path.display()
        );
    } else if backup_and_write_json(mcp_path, &settings) {
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed tokensave MCP server from {}",
            mcp_path.display()
        );
    }
}

// ---------------------------------------------------------------------------
// Healthcheck helpers
// ---------------------------------------------------------------------------

/// Check ~/.cursor/mcp.json has tokensave MCP server registered.
fn doctor_check_settings(dc: &mut DoctorCounters, home: &Path) {
    let mcp_path = home.join(".cursor/mcp.json");

    if !mcp_path.exists() {
        dc.warn(&format!(
            "{} not found — run `tokensave install --agent cursor` if you use Cursor",
            mcp_path.display()
        ));
        return;
    }

    let settings = load_json_file(&mcp_path);
    let server = settings.get("mcpServers").and_then(|v| v.get("tokensave"));

    if server.and_then(|v| v.as_object()).is_some() {
        dc.pass(&format!("MCP server registered in {}", mcp_path.display()));
    } else {
        dc.fail(&format!(
            "MCP server NOT registered in {} — run `tokensave install --agent cursor`",
            mcp_path.display()
        ));
    }
}
