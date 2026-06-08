//! Cursor agent integration.
//!
//! Handles registration of the tokensave MCP server in Cursor's
//! `~/.cursor/mcp.json` under the `mcpServers.tokensave` key.

use std::path::Path;

use serde_json::json;

use crate::errors::{Result, TokenSaveError};

use super::{
    backup_and_write_json, backup_config_file, load_json_file, load_json_file_strict,
    load_jsonc_file_strict, safe_write_json_file, tool_names, AgentIntegration, DoctorCounters,
    HealthcheckContext, InstallContext, InstallScope,
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
        install_mcp_server(
            &ctx.home.join(".cursor/mcp.json"),
            &ctx.tokensave_bin,
            InstallScope::Global,
        )?;

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
        for path in [
            cursor_dir.join("mcp.json"),
            cursor_dir.join("rules/tokensave.mdc"),
            cursor_dir.join("permissions.json"),
            cursor_dir.join("hooks.json"),
        ] {
            super::ensure_project_local_safe_path(project_path, &path)?;
        }
        install_mcp_server(
            &cursor_dir.join("mcp.json"),
            &ctx.tokensave_bin,
            InstallScope::ProjectLocal,
        )?;
        install_project_rule(&cursor_dir.join("rules/tokensave.mdc"))?;
        install_permissions(&cursor_dir.join("permissions.json"))?;
        install_hooks(&cursor_dir.join("hooks.json"), &ctx.tokensave_bin)
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
        let project_cursor = ctx.project_path.join(".cursor");
        if project_cursor.join("mcp.json").exists()
            || project_cursor.join("hooks.json").exists()
            || project_cursor.join("permissions.json").exists()
            || project_cursor.join("rules/tokensave.mdc").exists()
        {
            doctor_check_local_settings(dc, &project_cursor);
        } else {
            doctor_check_settings(dc, &ctx.home);
        }
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

fn install_mcp_server(mcp_path: &Path, tokensave_bin: &str, scope: InstallScope) -> Result<()> {
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
    let mut server = json!({
        "type": "stdio",
        "command": tokensave_bin,
        "args": ["serve"]
    });
    match scope {
        InstallScope::Global => {
            server["env"]["TOKENSAVE_ENABLE_GLOBAL_DB"] = json!("1");
        }
        InstallScope::ProjectLocal => {
            server["args"] = json!(["serve", "--path", "."]);
        }
    }
    settings["mcpServers"]["tokensave"] = server;

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

- For codebase exploration, symbol lookup, call graphs, callers/callees, impact analysis, affected files, and architectural navigation, use the tokensave MCP tools first. Treat `tokensave_context` as the default starting point before Grep/Glob/search when `.tokensave/` exists.
- Prefer tools such as `tokensave_context`, `tokensave_search`, `tokensave_callers`, `tokensave_callees`, `tokensave_impact`, `tokensave_files`, `tokensave_affected`, and related read-only tokensave tools before broad file reads or search.
- For durable project/user facts, prefer `tokensave_fact_store`, `tokensave_fact_feedback`, and `tokensave_memory_status` over ad-hoc notes. Use `tokensave_message_search` for project-local Cursor transcript recall when prior conversation context matters.
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

    let tokensave_entries = cursor_permission_entries();
    let known_tokensave_entries: std::collections::HashSet<String> =
        tokensave_entries.iter().cloned().collect();
    let existing = permissions["mcpAllowlist"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .filter(|entry| {
                    !entry.starts_with("tokensave:") || known_tokensave_entries.contains(entry)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut allow = existing;
    for entry in tokensave_entries {
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

fn cursor_permission_entries() -> Vec<String> {
    tool_names()
        .into_iter()
        .map(|tool| format!("tokensave:{tool}"))
        .collect()
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
        Some("Shell|Bash|Grep|Glob|Search"),
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
    // End-of-turn transcript ingestion. This is the primary, off-hot-path place
    // we capture Cursor transcripts (beforeSubmitPrompt only does a tiny tail
    // read), so it gets a generous timeout for the incremental catch-up.
    install_cursor_hook_entry(
        &mut hooks,
        "stop",
        tokensave_bin,
        "hook-cursor-stop",
        30,
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
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| TokenSaveError::Config {
            message: format!("failed to create {}: {e}", parent.display()),
        })?;
    }
    std::fs::write(path, contents).map_err(|e| TokenSaveError::Config {
        message: format!("failed to write {}: {e}", path.display()),
    })
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
    doctor_check_mcp_server(
        dc,
        &mcp_path,
        "`tokensave install --agent cursor`",
        "global",
    );
}

fn doctor_check_local_settings(dc: &mut DoctorCounters, cursor_dir: &Path) {
    doctor_check_mcp_server(
        dc,
        &cursor_dir.join("mcp.json"),
        "`tokensave install --local --agent cursor`",
        "project-local",
    );
    doctor_check_permissions(dc, &cursor_dir.join("permissions.json"));
    doctor_check_hooks(dc, &cursor_dir.join("hooks.json"));
    doctor_check_rule(dc, &cursor_dir.join("rules/tokensave.mdc"));
}

fn doctor_check_mcp_server(dc: &mut DoctorCounters, mcp_path: &Path, fix: &str, label: &str) {
    if !mcp_path.exists() {
        dc.warn(&format!(
            "{} not found — run {fix} if you use Cursor",
            mcp_path.display()
        ));
        return;
    }

    let settings = load_json_file(mcp_path);
    let server = settings.get("mcpServers").and_then(|v| v.get("tokensave"));

    if server.and_then(|v| v.as_object()).is_some() {
        dc.pass(&format!("MCP server registered in {}", mcp_path.display()));
    } else {
        dc.fail(&format!(
            "{label} MCP server NOT registered in {} — run {fix}",
            mcp_path.display()
        ));
    }
}

fn doctor_check_permissions(dc: &mut DoctorCounters, permissions_path: &Path) {
    if !permissions_path.exists() {
        dc.warn(&format!(
            "{} not found — run `tokensave install --local --agent cursor`",
            permissions_path.display()
        ));
        return;
    }
    let permissions = load_jsonc_file_strict(permissions_path).unwrap_or_else(|e| {
        dc.fail(&format!("{e}"));
        json!({})
    });
    let installed: std::collections::HashSet<&str> = permissions["mcpAllowlist"]
        .as_array()
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    let expected = cursor_permission_entries();
    let expected_set: std::collections::HashSet<&str> =
        expected.iter().map(String::as_str).collect();
    let missing = expected
        .iter()
        .filter(|entry| !installed.contains(entry.as_str()))
        .count();
    let stale = installed
        .iter()
        .filter(|entry| entry.starts_with("tokensave:") && !expected_set.contains(*entry))
        .count();
    if missing == 0 && stale == 0 {
        dc.pass(&format!(
            "All {} Cursor MCP permissions granted in {}",
            expected.len(),
            permissions_path.display()
        ));
    } else {
        dc.fail(&format!(
            "{missing} Cursor MCP permission(s) missing and {stale} stale — run `tokensave install --local --agent cursor`"
        ));
    }
}

fn doctor_check_hooks(dc: &mut DoctorCounters, hooks_path: &Path) {
    if !hooks_path.exists() {
        dc.warn(&format!(
            "{} not found — run `tokensave install --local --agent cursor`",
            hooks_path.display()
        ));
        return;
    }
    let hooks = load_jsonc_file_strict(hooks_path).unwrap_or_else(|e| {
        dc.fail(&format!("{e}"));
        json!({})
    });
    let expected = [
        ("sessionStart", "hook-cursor-session-start"),
        ("subagentStart", "hook-cursor-subagent-start"),
        ("preToolUse", "hook-cursor-pre-tool-use"),
        ("beforeSubmitPrompt", "hook-cursor-before-submit-prompt"),
        ("afterFileEdit", "hook-cursor-after-file-edit"),
        ("afterShellExecution", "hook-cursor-after-shell"),
        ("workspaceOpen", "hook-cursor-workspace-open"),
        ("stop", "hook-cursor-stop"),
    ];
    let missing: Vec<&str> = expected
        .iter()
        .filter_map(|(event, command)| {
            let has = hooks["hooks"][*event].as_array().is_some_and(|entries| {
                entries.iter().any(|entry| {
                    entry["command"]
                        .as_str()
                        .is_some_and(|value| value.contains(command))
                })
            });
            (!has).then_some(*event)
        })
        .collect();
    if missing.is_empty() {
        dc.pass(&format!(
            "All {} Cursor lifecycle hooks registered in {}",
            expected.len(),
            hooks_path.display()
        ));
    } else {
        dc.fail(&format!(
            "Cursor hook(s) missing for {} — run `tokensave install --local --agent cursor`",
            missing.join(", ")
        ));
    }
}

fn doctor_check_rule(dc: &mut DoctorCounters, rule_path: &Path) {
    if !rule_path.exists() {
        dc.warn(&format!(
            "{} not found — run `tokensave install --local --agent cursor`",
            rule_path.display()
        ));
        return;
    }
    let contents = std::fs::read_to_string(rule_path).unwrap_or_default();
    if contents.contains("alwaysApply: true") && contents.contains("tokensave MCP tools") {
        dc.pass(&format!(
            "Cursor tokensave rule active in {}",
            rule_path.display()
        ));
    } else {
        dc.fail(&format!(
            "Cursor tokensave rule is incomplete in {} — run `tokensave install --local --agent cursor`",
            rule_path.display()
        ));
    }
}
