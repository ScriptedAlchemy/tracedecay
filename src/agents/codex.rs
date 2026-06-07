// Rust guideline compliant 2025-10-17
//! `OpenAI` Codex CLI agent integration.
//!
//! Handles registration of the tokensave MCP server in Codex's config
//! file (`~/.codex/config.toml`), per-tool auto-approval settings, prompt
//! rules via `AGENTS.md`, and lifecycle hooks via `hooks.json`.
//!
//! Codex supports a Claude-style lifecycle hook system (`SessionStart`,
//! `UserPromptSubmit`, `SubagentStart`, `PostToolUse`, …). Hooks are enabled by
//! default, but non-managed command hooks must be reviewed and trusted with the
//! `/hooks` CLI before they run — newly installed or changed hooks are skipped
//! until trusted. The installer prints that guidance after writing `hooks.json`.

use std::io::Write;
use std::path::Path;

use serde_json::json;

use crate::errors::{Result, TokenSaveError};

use super::{
    backup_config_file, load_json_file_strict, load_toml_file, safe_write_json_file, tool_names,
    write_toml_file, AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext,
};

/// `OpenAI` Codex CLI agent.
pub struct CodexIntegration;

impl AgentIntegration for CodexIntegration {
    fn name(&self) -> &'static str {
        "Codex CLI"
    }

    fn id(&self) -> &'static str {
        "codex"
    }

    fn install(&self, ctx: &InstallContext) -> Result<()> {
        let codex_dir = ctx.home.join(".codex");
        std::fs::create_dir_all(&codex_dir).ok();
        let config_path = codex_dir.join("config.toml");

        install_mcp_server(&config_path, &ctx.tokensave_bin, None)?;

        let agents_md = codex_dir.join("AGENTS.md");
        install_prompt_rules(&agents_md)?;

        install_hooks(&codex_dir.join("hooks.json"), &ctx.tokensave_bin)?;

        eprintln!();
        eprintln!("Setup complete. Next steps:");
        eprintln!("  1. cd into your project and run: tokensave init");
        eprintln!("  2. Start a new Codex session — tokensave tools are now available");
        print_hook_trust_guidance();
        Ok(())
    }

    fn supports_local_install(&self) -> bool {
        true
    }

    fn install_local(&self, ctx: &InstallContext, project_path: &Path) -> Result<()> {
        let codex_dir = project_path.join(".codex");
        std::fs::create_dir_all(&codex_dir).ok();
        install_mcp_server(
            &codex_dir.join("config.toml"),
            &ctx.tokensave_bin,
            Some(project_path),
        )?;
        install_prompt_rules(&project_path.join("AGENTS.md"))?;
        install_hooks(&codex_dir.join("hooks.json"), &ctx.tokensave_bin)?;
        print_hook_trust_guidance();
        Ok(())
    }

    fn uninstall(&self, ctx: &InstallContext) -> Result<()> {
        let codex_dir = ctx.home.join(".codex");
        let config_path = codex_dir.join("config.toml");

        uninstall_mcp_server(&config_path)?;

        let agents_md = codex_dir.join("AGENTS.md");
        uninstall_prompt_rules(&agents_md);

        uninstall_hooks(&codex_dir.join("hooks.json"));

        eprintln!();
        eprintln!("Uninstall complete. Tokensave has been removed from Codex CLI.");
        eprintln!("Start a new Codex session for changes to take effect.");
        Ok(())
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mCodex CLI integration\x1b[0m");
        let local_codex_dir = ctx.project_path.join(".codex");
        if local_codex_dir.join("config.toml").exists()
            || local_codex_dir.join("hooks.json").exists()
            || ctx.project_path.join("AGENTS.md").exists()
        {
            doctor_check_config(dc, &local_codex_dir.join("config.toml"));
            doctor_check_prompt_file(dc, &ctx.project_path.join("AGENTS.md"));
            doctor_check_hooks(dc, &local_codex_dir.join("hooks.json"));
        } else {
            let codex_dir = ctx.home.join(".codex");
            let config_path = codex_dir.join("config.toml");
            doctor_check_config(dc, &config_path);
            doctor_check_prompt_file(dc, &codex_dir.join("AGENTS.md"));
            doctor_check_hooks(dc, &codex_dir.join("hooks.json"));
        }
    }

    fn is_detected(&self, home: &Path) -> bool {
        home.join(".codex").is_dir()
    }

    fn primary_config_path(&self, home: &Path) -> Option<std::path::PathBuf> {
        Some(home.join(".codex/config.toml"))
    }

    fn has_tokensave(&self, home: &Path) -> bool {
        let config = home.join(".codex").join("config.toml");
        if !config.exists() {
            return false;
        }
        // If the file is unparseable, conservatively report "not installed"
        // so the caller treats it like a fresh install path.
        super::load_toml_file(&config).is_ok_and(|toml| {
            toml.get("mcp_servers")
                .and_then(|v| v.get("tokensave"))
                .is_some()
        })
    }
}

// ---------------------------------------------------------------------------
// Install helpers
// ---------------------------------------------------------------------------

/// Register MCP server and auto-approve tools in ~/.codex/config.toml.
fn install_mcp_server(
    config_path: &Path,
    tokensave_bin: &str,
    local_project_path: Option<&Path>,
) -> Result<()> {
    let mut config = load_toml_file(config_path)?;

    // Ensure [mcp_servers.tokensave] exists
    let table = config
        .as_table_mut()
        .ok_or_else(|| TokenSaveError::Config {
            message: "config.toml is not a TOML table".to_string(),
        })?;

    let servers = table
        .entry("mcp_servers")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or_else(|| TokenSaveError::Config {
            message: "mcp_servers is not a table in config.toml".to_string(),
        })?;

    let mut server_table = toml::map::Map::new();
    server_table.insert(
        "command".to_string(),
        toml::Value::String(tokensave_bin.to_string()),
    );
    let args = if let Some(project_path) = local_project_path {
        vec![
            toml::Value::String("serve".to_string()),
            toml::Value::String("--path".to_string()),
            toml::Value::String(project_path.to_string_lossy().to_string()),
        ]
    } else {
        vec![toml::Value::String("serve".to_string())]
    };
    server_table.insert("args".to_string(), toml::Value::Array(args));
    if local_project_path.is_some() {
        let mut env_table = toml::map::Map::new();
        env_table.insert(
            "TOKENSAVE_DISABLE_GLOBAL_DB".to_string(),
            toml::Value::String("1".to_string()),
        );
        server_table.insert("env".to_string(), toml::Value::Table(env_table));
    }

    // Auto-approve all tokensave tools so Codex doesn't prompt for each one
    let mut tools_table = toml::map::Map::new();
    for tool_name in tool_names() {
        let mut tool_config = toml::map::Map::new();
        tool_config.insert(
            "approval_mode".to_string(),
            toml::Value::String("auto".to_string()),
        );
        tools_table.insert(tool_name.clone(), toml::Value::Table(tool_config));
    }
    server_table.insert("tools".to_string(), toml::Value::Table(tools_table));

    servers.insert("tokensave".to_string(), toml::Value::Table(server_table));

    write_toml_file(config_path, &config)?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Added tokensave MCP server to {}",
        config_path.display()
    );
    Ok(())
}

/// Append prompt rules to AGENTS.md (idempotent).
fn install_prompt_rules(agents_md: &Path) -> Result<()> {
    let marker = "## Prefer tokensave MCP tools";
    let existing = if agents_md.exists() {
        std::fs::read_to_string(agents_md).unwrap_or_default()
    } else {
        String::new()
    };
    if existing.contains(marker) {
        eprintln!("  AGENTS.md already contains tokensave rules, skipping");
        return Ok(());
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(agents_md)
        .map_err(|e| TokenSaveError::Config {
            message: format!("failed to open AGENTS.md: {e}"),
        })?;
    write!(
        f,
        "\n{marker}\n\n\
        Before reading source files or scanning the codebase, use the tokensave MCP tools \
        (`tokensave_context`, `tokensave_search`, `tokensave_callers`, `tokensave_callees`, \
        `tokensave_impact`, `tokensave_node`, `tokensave_files`, `tokensave_affected`). \
        They provide instant semantic results from a pre-built knowledge graph and are \
        faster than file reads.\n\n\
        If a code analysis question cannot be fully answered by tokensave MCP tools, \
        try querying the SQLite database directly at `.tokensave/tokensave.db` \
        (tables: `nodes`, `edges`, `files`). Use SQL to answer complex structural queries \
        that go beyond what the built-in tools expose.\n\n\
        If you discover a gap where an extractor, schema, or tokensave tool could be \
        improved to answer a question natively, propose to the user that they open an issue \
        at https://github.com/aovestdipaperino/tokensave describing the limitation. \
        **Remind the user to strip any sensitive or proprietary code from the bug description \
        before submitting.**\n"
    )
    .ok();
    eprintln!(
        "\x1b[32m✔\x1b[0m Appended tokensave rules to {}",
        agents_md.display()
    );
    Ok(())
}

/// Register tokensave lifecycle hooks in a Codex `hooks.json` (idempotent).
///
/// Codex organizes hooks as `hooks[event][] -> { matcher?, hooks: [handler] }`
/// where only `type: "command"` handlers run today and `timeout` is in seconds.
/// We register the tokensave-owned group for each event, preserving any foreign
/// groups, so reinstalls reconcile in place. Backs up an existing file first.
fn install_hooks(hooks_path: &Path, tokensave_bin: &str) -> Result<()> {
    let backup = backup_config_file(hooks_path)?;
    let mut hooks = match load_json_file_strict(hooks_path) {
        Ok(v) => v,
        Err(e) => {
            if let Some(ref b) = backup {
                eprintln!("  Backup preserved at: {}", b.display());
            }
            return Err(e);
        }
    };

    // Steer the agent toward tokensave MCP tools + report index freshness.
    install_codex_hook_event(
        &mut hooks,
        "SessionStart",
        tokensave_bin,
        "hook-codex-session-start",
        5,
        None,
    );
    install_codex_hook_event(
        &mut hooks,
        "UserPromptSubmit",
        tokensave_bin,
        "hook-codex-user-prompt-submit",
        5,
        None,
    );
    install_codex_hook_event(
        &mut hooks,
        "SubagentStart",
        tokensave_bin,
        "hook-codex-subagent-start",
        5,
        None,
    );
    // Keep the index fresh: apply_patch → targeted single-file sync; Bash →
    // branch-aware / incremental sync. Matcher targets both tool names.
    install_codex_hook_event(
        &mut hooks,
        "PostToolUse",
        tokensave_bin,
        "hook-codex-post-tool-use",
        60,
        Some("Bash|apply_patch"),
    );

    safe_write_json_file(hooks_path, &hooks, backup.as_deref())?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Added Codex lifecycle hooks to {}",
        hooks_path.display()
    );
    Ok(())
}

/// Insert (or reconcile) the tokensave-owned matcher group for `event`.
///
/// Drops any pre-existing group that already contains our `subcommand` handler
/// (so refinements to matcher/timeout reach old configs) while preserving every
/// foreign group. Idempotent: exactly one tokensave group per event.
fn install_codex_hook_event(
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
    let mut groups: Vec<serde_json::Value> = existing
        .into_iter()
        .filter(|group| !group_has_subcommand(group, subcommand))
        .collect();

    let handler = json!({
        "type": "command",
        "command": format!("{} {subcommand}", shell_quote(tokensave_bin)),
        "timeout": timeout,
    });
    let mut group = json!({ "hooks": [handler] });
    if let Some(matcher) = matcher {
        group["matcher"] = json!(matcher);
    }
    groups.push(group);

    hooks["hooks"][event] = serde_json::Value::Array(groups);
}

/// True when any handler command in `group` contains `subcommand`.
fn group_has_subcommand(group: &serde_json::Value, subcommand: &str) -> bool {
    group["hooks"].as_array().is_some_and(|handlers| {
        handlers.iter().any(|h| {
            h.get("command")
                .and_then(|c| c.as_str())
                .is_some_and(|command| command.contains(subcommand))
        })
    })
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Codex requires non-managed command hooks to be trusted via `/hooks` before
/// they run; newly installed/changed hooks are skipped until trusted.
fn print_hook_trust_guidance() {
    eprintln!();
    eprintln!(
        "\x1b[1mAction required:\x1b[0m Codex skips new/changed command hooks until you trust them."
    );
    eprintln!("  Run \x1b[1m/hooks\x1b[0m inside Codex to review and trust the tokensave hooks.");
    eprintln!(
        "  (For one-off non-interactive runs you can pass --dangerously-bypass-hook-trust, \
         but trusting via /hooks is recommended.)"
    );
}

// ---------------------------------------------------------------------------
// Uninstall helpers
// ---------------------------------------------------------------------------

/// Remove tokensave-owned hook groups from a Codex `hooks.json`.
fn uninstall_hooks(hooks_path: &Path) {
    const SUBCOMMANDS: [&str; 4] = [
        "hook-codex-session-start",
        "hook-codex-user-prompt-submit",
        "hook-codex-subagent-start",
        "hook-codex-post-tool-use",
    ];

    if !hooks_path.exists() {
        return;
    }
    let Ok(mut hooks) = load_json_file_strict(hooks_path) else {
        return;
    };

    let Some(events) = hooks.get_mut("hooks").and_then(|h| h.as_object_mut()) else {
        return;
    };
    for groups in events.values_mut() {
        if let Some(arr) = groups.as_array_mut() {
            arr.retain(|group| !SUBCOMMANDS.iter().any(|sc| group_has_subcommand(group, sc)));
        }
    }
    events.retain(|_, groups| groups.as_array().is_some_and(|a| !a.is_empty()));

    let is_empty = hooks
        .get("hooks")
        .and_then(|h| h.as_object())
        .is_some_and(serde_json::Map::is_empty);
    if is_empty {
        std::fs::remove_file(hooks_path).ok();
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed {} (was empty)",
            hooks_path.display()
        );
    } else if safe_write_json_file(hooks_path, &hooks, None).is_ok() {
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed tokensave hooks from {}",
            hooks_path.display()
        );
    }
}

/// Remove MCP server from ~/.codex/config.toml.
fn uninstall_mcp_server(config_path: &Path) -> Result<()> {
    if !config_path.exists() {
        return Ok(());
    }
    let mut config = load_toml_file(config_path)?;
    let Some(table) = config.as_table_mut() else {
        return Ok(());
    };
    let Some(servers) = table.get_mut("mcp_servers").and_then(|v| v.as_table_mut()) else {
        return Ok(());
    };
    if servers.remove("tokensave").is_none() {
        eprintln!(
            "  No tokensave MCP server in {}, skipping",
            config_path.display()
        );
        return Ok(());
    }
    if servers.is_empty() {
        table.remove("mcp_servers");
    }
    if table.is_empty() {
        std::fs::remove_file(config_path).ok();
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed {} (was empty)",
            config_path.display()
        );
    } else {
        write_toml_file(config_path, &config)?;
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed tokensave MCP server from {}",
            config_path.display()
        );
    }
    Ok(())
}

/// Remove tokensave rules from AGENTS.md.
fn uninstall_prompt_rules(agents_md: &Path) {
    if !agents_md.exists() {
        return;
    }
    let Ok(contents) = std::fs::read_to_string(agents_md) else {
        return;
    };
    if !contents.contains("tokensave") {
        eprintln!("  AGENTS.md does not contain tokensave rules, skipping");
        return;
    }
    let marker = "## Prefer tokensave MCP tools";
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
        std::fs::remove_file(agents_md).ok();
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed {} (was empty)",
            agents_md.display()
        );
    } else {
        std::fs::write(agents_md, format!("{new_contents}\n")).ok();
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed tokensave rules from {}",
            agents_md.display()
        );
    }
}

// ---------------------------------------------------------------------------
// Healthcheck helpers
// ---------------------------------------------------------------------------

/// Check config.toml has tokensave registered.
fn doctor_check_config(dc: &mut DoctorCounters, config_path: &Path) {
    if !config_path.exists() {
        dc.warn(&format!(
            "{} not found — run `tokensave install --agent codex` if you use Codex CLI",
            config_path.display()
        ));
        return;
    }

    let config = match load_toml_file(config_path) {
        Ok(c) => c,
        Err(e) => {
            dc.fail(&format!("{e}"));
            return;
        }
    };
    let has_server = config
        .get("mcp_servers")
        .and_then(|v| v.get("tokensave"))
        .and_then(|v| v.as_table())
        .is_some();

    if !has_server {
        dc.fail(&format!(
            "MCP server NOT registered in {} — run `tokensave install --agent codex`",
            config_path.display()
        ));
        return;
    }
    dc.pass(&format!(
        "MCP server registered in {}",
        config_path.display()
    ));

    // Check tool auto-approval
    let tools = config
        .get("mcp_servers")
        .and_then(|v| v.get("tokensave"))
        .and_then(|v| v.get("tools"))
        .and_then(|v| v.as_table());

    let auto_count = tools.map_or(0, |t| {
        t.values()
            .filter(|v| v.get("approval_mode").and_then(|m| m.as_str()) == Some("auto"))
            .count()
    });

    let tools = tool_names();
    let tools_len = tools.len();
    if auto_count >= tools_len {
        dc.pass(&format!("All {tools_len} tools set to auto-approve"));
    } else if auto_count > 0 {
        dc.warn(&format!(
            "{auto_count}/{tools_len} tools auto-approved — run `tokensave install --agent codex` to update"
        ));
    } else {
        dc.warn("No tools auto-approved — Codex will prompt for each tool call");
    }
}

/// Check AGENTS.md contains tokensave rules.
fn doctor_check_prompt_file(dc: &mut DoctorCounters, agents_md: &Path) {
    if agents_md.exists() {
        let has_rules = std::fs::read_to_string(&agents_md)
            .unwrap_or_default()
            .contains("tokensave");
        if has_rules {
            dc.pass(&format!(
                "AGENTS.md contains tokensave rules in {}",
                agents_md.display()
            ));
        } else {
            dc.fail(&format!(
                "AGENTS.md missing tokensave rules in {} — run `tokensave install --local --agent codex` or `tokensave install --agent codex`",
                agents_md.display()
            ));
        }
    } else {
        dc.warn(&format!("{} does not exist", agents_md.display()));
    }
}

/// Check hooks.json registers the tokensave lifecycle hooks, and remind the
/// user that Codex requires trusting them via `/hooks` before they run.
fn doctor_check_hooks(dc: &mut DoctorCounters, hooks_path: &Path) {
    if !hooks_path.exists() {
        dc.warn(&format!(
            "{} not found — run `tokensave install --agent codex` to add lifecycle hooks",
            hooks_path.display()
        ));
        return;
    }
    let hooks = super::load_json_file(hooks_path);
    let expected = [
        ("SessionStart", "hook-codex-session-start"),
        ("UserPromptSubmit", "hook-codex-user-prompt-submit"),
        ("SubagentStart", "hook-codex-subagent-start"),
        ("PostToolUse", "hook-codex-post-tool-use"),
    ];
    let missing: Vec<&str> = expected
        .iter()
        .filter_map(|(event, command)| {
            (!codex_hook_present(&hooks, event, command)).then_some(*event)
        })
        .collect();
    if missing.is_empty() {
        dc.pass(&format!(
            "All {} Codex lifecycle hooks registered in {}",
            expected.len(),
            hooks_path.display()
        ));
        dc.info(
            "Codex skips new/changed command hooks until trusted — run `/hooks` in Codex to trust the tokensave hooks",
        );
    } else {
        dc.warn(&format!(
            "tokensave hook(s) missing for {} in {} — run `tokensave install --local --agent codex` or `tokensave install --agent codex`",
            missing.join(", "),
            hooks_path.display(),
        ));
    }
}

fn codex_hook_present(hooks: &serde_json::Value, event: &str, command: &str) -> bool {
    hooks["hooks"][event].as_array().is_some_and(|groups| {
        groups.iter().any(|group| {
            group["hooks"].as_array().is_some_and(|handlers| {
                handlers.iter().any(|h| {
                    h["command"]
                        .as_str()
                        .is_some_and(|value| value.contains(command))
                })
            })
        })
    })
}
