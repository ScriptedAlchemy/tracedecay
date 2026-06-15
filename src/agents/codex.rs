// Rust guideline compliant 2025-10-17
//! `OpenAI` Codex CLI agent integration.
//!
//! Handles registration of the tracedecay MCP server in Codex's config
//! file (`~/.codex/config.toml`), per-tool auto-approval settings, prompt
//! rules via `AGENTS.md`, and lifecycle hooks via `hooks.json`.
//!
//! Codex supports a Claude-style lifecycle hook system (`SessionStart`,
//! `UserPromptSubmit`, `SubagentStart`, `PostToolUse`, …). Hooks are enabled by
//! default, but non-managed command hooks must be reviewed and trusted with the
//! `/hooks` CLI before they run — newly installed or changed hooks are skipped
//! until trusted. The installer prints that guidance after writing `hooks.json`.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::errors::{Result, TraceDecayError};

use super::{
    backup_config_file, load_json_file, load_json_file_strict, load_toml_file,
    safe_write_json_file, safe_write_text_file, tool_names, write_toml_file, AgentIntegration,
    DoctorCounters, HealthcheckContext, InstallContext, InstallScope, UpdatePluginOutcome,
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
        install_codex_plugin(&ctx.home, &ctx.tracedecay_bin)?;

        let agents_md = codex_dir.join("AGENTS.md");
        install_prompt_rules(&agents_md)?;

        install_hooks(&codex_dir.join("hooks.json"), &ctx.tracedecay_bin)?;

        eprintln!();
        eprintln!("Setup complete. Next steps:");
        eprintln!("  1. cd into your project and run: tracedecay init");
        eprintln!("  2. In Codex, run: codex plugin add tracedecay@personal");
        eprintln!("  3. Start a new Codex session — tracedecay tools are now available");
        print_hook_trust_guidance();
        Ok(())
    }

    fn supports_local_install(&self) -> bool {
        true
    }

    fn install_local(&self, ctx: &InstallContext, project_path: &Path) -> Result<()> {
        let codex_dir = project_path.join(".codex");
        for path in [
            codex_dir.join("config.toml"),
            codex_dir.join("hooks.json"),
            project_path.join("AGENTS.md"),
        ] {
            super::ensure_project_local_safe_path(project_path, &path)?;
        }
        std::fs::create_dir_all(&codex_dir).ok();
        install_mcp_server(
            &codex_dir.join("config.toml"),
            &ctx.tracedecay_bin,
            InstallScope::ProjectLocal,
        )?;
        install_prompt_rules(&project_path.join("AGENTS.md"))?;
        install_hooks(&codex_dir.join("hooks.json"), &ctx.tracedecay_bin)?;
        print_hook_trust_guidance();
        Ok(())
    }

    fn uninstall(&self, ctx: &InstallContext) -> Result<()> {
        let codex_dir = ctx.home.join(".codex");
        let config_path = codex_dir.join("config.toml");

        uninstall_mcp_server(&config_path)?;
        uninstall_codex_plugin(&ctx.home)?;

        let agents_md = codex_dir.join("AGENTS.md");
        uninstall_prompt_rules(&agents_md);

        uninstall_hooks(&codex_dir.join("hooks.json"));

        eprintln!();
        eprintln!("Uninstall complete. TraceDecay has been removed from Codex CLI.");
        eprintln!("Start a new Codex session for changes to take effect.");
        Ok(())
    }

    fn update_plugin(&self, ctx: &InstallContext) -> Result<UpdatePluginOutcome> {
        let plugin_dir = codex_plugin_install_dir(&ctx.home);
        let legacy_dir = codex_plugin_legacy_install_dir(&ctx.home);
        let target = if codex_plugin_manifest_path(&ctx.home).exists() {
            Some(plugin_dir)
        } else if codex_plugin_legacy_manifest_path(&ctx.home).exists() {
            Some(legacy_dir)
        } else if Self::has_legacy_config_install(&ctx.home) {
            return Ok(UpdatePluginOutcome::ConfigOnly);
        } else {
            None
        };

        let Some(target) = target else {
            return Ok(UpdatePluginOutcome::NotInstalled);
        };
        write_codex_plugin_files(&target, &ctx.tracedecay_bin)?;
        Ok(UpdatePluginOutcome::Refreshed(vec![target]))
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mCodex CLI integration\x1b[0m");
        let local_codex_dir = ctx.project_path.join(".codex");
        if local_codex_dir.join("config.toml").exists()
            || local_codex_dir.join("hooks.json").exists()
            || local_agents_md_has_tracedecay(&ctx.project_path.join("AGENTS.md"))
        {
            doctor_check_config(dc, &local_codex_dir.join("config.toml"));
            doctor_check_prompt_file(dc, &ctx.project_path.join("AGENTS.md"));
            doctor_check_hooks(dc, &local_codex_dir.join("hooks.json"));
        } else {
            let codex_dir = ctx.home.join(".codex");
            doctor_check_plugin(dc, &ctx.home);
            doctor_check_prompt_file(dc, &codex_dir.join("AGENTS.md"));
            doctor_check_hooks(dc, &codex_dir.join("hooks.json"));
        }
    }

    fn is_detected(&self, home: &Path) -> bool {
        home.join(".codex").is_dir()
    }

    fn primary_config_path(&self, home: &Path) -> Option<std::path::PathBuf> {
        Some(codex_plugin_manifest_path(home))
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        if codex_plugin_manifest_path(home).exists() {
            return true;
        }
        Self::has_legacy_config_install(home)
    }
}

impl CodexIntegration {
    fn has_legacy_config_install(home: &Path) -> bool {
        let config = home.join(".codex").join("config.toml");
        if !config.exists() {
            return false;
        }
        // If the file is unparseable, conservatively report "not installed"
        // so the caller treats it like a fresh install path.
        super::load_toml_file(&config).is_ok_and(|toml| {
            toml.get("mcp_servers")
                .and_then(|v| v.get("tracedecay"))
                .is_some()
        })
    }
}

fn local_agents_md_has_tracedecay(path: &Path) -> bool {
    path.exists()
        && std::fs::read_to_string(path)
            .unwrap_or_default()
            .contains("## Prefer tracedecay MCP tools")
}

// ---------------------------------------------------------------------------
// Install helpers
// ---------------------------------------------------------------------------

const CODEX_EMBEDDED_PLUGIN_FILES: &[(&str, &str)] = &[
    (
        ".codex-plugin/plugin.json",
        include_str!("../../codex-plugin/.codex-plugin/plugin.json"),
    ),
    (".mcp.json", include_str!("../../codex-plugin/.mcp.json")),
    ("README.md", include_str!("../../codex-plugin/README.md")),
    (
        "skills/reading-code-cheaply/SKILL.md",
        include_str!("../../codex-plugin/skills/reading-code-cheaply/SKILL.md"),
    ),
];

fn codex_plugin_install_dir(home: &Path) -> PathBuf {
    home.join("plugins/tracedecay")
}

fn codex_plugin_legacy_install_dir(home: &Path) -> PathBuf {
    home.join("plugins/tokensave")
}

fn codex_plugin_manifest_path(home: &Path) -> PathBuf {
    codex_plugin_install_dir(home).join(".codex-plugin/plugin.json")
}

fn codex_plugin_legacy_manifest_path(home: &Path) -> PathBuf {
    codex_plugin_legacy_install_dir(home).join(".codex-plugin/plugin.json")
}

fn codex_personal_marketplace_path(home: &Path) -> PathBuf {
    home.join(".agents/plugins/marketplace.json")
}

fn install_codex_plugin(home: &Path, tracedecay_bin: &str) -> Result<()> {
    let install_dir = codex_plugin_install_dir(home);
    if let Some(parent) = install_dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| TraceDecayError::Config {
            message: format!("failed to create {}: {e}", parent.display()),
        })?;
    }
    remove_codex_plugin_install(&install_dir)?;
    remove_codex_plugin_install(&codex_plugin_legacy_install_dir(home))?;
    write_codex_plugin_files(&install_dir, tracedecay_bin)?;
    install_codex_marketplace_entry(home)?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Installed Codex plugin source at {}",
        install_dir.display()
    );
    Ok(())
}

fn write_codex_plugin_files(install_dir: &Path, tracedecay_bin: &str) -> Result<()> {
    for &(relative, contents) in CODEX_EMBEDDED_PLUGIN_FILES {
        let rendered = match relative {
            ".codex-plugin/plugin.json" => codex_plugin_manifest(contents)?,
            ".mcp.json" => codex_plugin_mcp(contents, tracedecay_bin)?,
            _ => contents.to_string(),
        };
        safe_write_text_file(&install_dir.join(relative), &rendered, None)?;
    }
    Ok(())
}

fn codex_plugin_manifest(raw: &str) -> Result<String> {
    let mut manifest: serde_json::Value = serde_json::from_str(raw)?;
    manifest["version"] = json!(env!("CARGO_PKG_VERSION"));
    Ok(format!("{}\n", serde_json::to_string_pretty(&manifest)?))
}

fn codex_plugin_mcp(raw: &str, tracedecay_bin: &str) -> Result<String> {
    let mut mcp: serde_json::Value = serde_json::from_str(raw)?;
    mcp["mcpServers"]["tracedecay"]["command"] = json!(tracedecay_bin);
    Ok(format!("{}\n", serde_json::to_string_pretty(&mcp)?))
}

fn install_codex_marketplace_entry(home: &Path) -> Result<()> {
    let marketplace_path = codex_personal_marketplace_path(home);
    let mut marketplace = load_json_file_strict(&marketplace_path)?;
    if !marketplace.is_object() {
        marketplace = json!({});
    }
    if marketplace
        .get("name")
        .and_then(|value| value.as_str())
        .is_none()
    {
        marketplace["name"] = json!("personal");
    }
    if !marketplace
        .get("interface")
        .is_some_and(serde_json::Value::is_object)
    {
        marketplace["interface"] = json!({ "displayName": "Personal" });
    } else if marketplace["interface"]
        .get("displayName")
        .and_then(|value| value.as_str())
        .is_none()
    {
        marketplace["interface"]["displayName"] = json!("Personal");
    }
    if !marketplace
        .get("plugins")
        .is_some_and(serde_json::Value::is_array)
    {
        marketplace["plugins"] = json!([]);
    }
    let Some(plugins) = marketplace["plugins"].as_array_mut() else {
        return Err(TraceDecayError::Config {
            message: "failed to normalize Codex marketplace plugins to an array".to_string(),
        });
    };
    plugins.retain(|entry| {
        !matches!(
            entry.get("name").and_then(|value| value.as_str()),
            Some("tracedecay" | "tokensave")
        )
    });
    plugins.push(json!({
        "name": "tracedecay",
        "source": {
            "source": "local",
            "path": "./plugins/tracedecay",
        },
        "policy": {
            "installation": "AVAILABLE",
            "authentication": "ON_INSTALL",
        },
        "category": "Productivity",
    }));
    safe_write_json_file(&marketplace_path, &marketplace, None)?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Added tracedecay to Codex personal marketplace at {}",
        marketplace_path.display()
    );
    Ok(())
}

fn uninstall_codex_plugin(home: &Path) -> Result<()> {
    remove_codex_plugin_install(&codex_plugin_install_dir(home))?;
    remove_codex_plugin_install(&codex_plugin_legacy_install_dir(home))?;
    remove_codex_marketplace_entry(home)?;
    Ok(())
}

fn remove_codex_plugin_install(install_dir: &Path) -> Result<()> {
    let Ok(metadata) = std::fs::symlink_metadata(install_dir) else {
        return Ok(());
    };
    if metadata.file_type().is_symlink() || metadata.is_file() {
        std::fs::remove_file(install_dir).map_err(|e| TraceDecayError::Config {
            message: format!("failed to remove {}: {e}", install_dir.display()),
        })?;
        return Ok(());
    }
    if !metadata.is_dir() {
        return Err(TraceDecayError::Config {
            message: format!(
                "refusing to replace non-directory Codex plugin path {}",
                install_dir.display()
            ),
        });
    }
    if !codex_plugin_dir_is_tracedecay(install_dir) {
        return Err(TraceDecayError::Config {
            message: format!(
                "refusing to replace unmanaged Codex plugin directory {}",
                install_dir.display()
            ),
        });
    }
    if codex_plugin_dir_has_only_managed_files(install_dir) {
        std::fs::remove_dir_all(install_dir).map_err(|e| TraceDecayError::Config {
            message: format!("failed to remove {}: {e}", install_dir.display()),
        })?;
    } else {
        for path in codex_plugin_managed_paths(install_dir) {
            std::fs::remove_file(&path).ok();
        }
    }
    Ok(())
}

fn codex_plugin_dir_is_tracedecay(install_dir: &Path) -> bool {
    let manifest = load_json_file(&install_dir.join(".codex-plugin/plugin.json"));
    matches!(
        manifest.get("name").and_then(|value| value.as_str()),
        Some("tracedecay" | "tokensave")
    )
}

fn codex_plugin_dir_has_only_managed_files(install_dir: &Path) -> bool {
    let Ok(entries) = collect_regular_files(install_dir) else {
        return false;
    };
    let managed = codex_plugin_managed_paths(install_dir);
    entries.iter().all(|entry| managed.contains(entry))
}

fn codex_plugin_managed_paths(install_dir: &Path) -> Vec<PathBuf> {
    CODEX_EMBEDDED_PLUGIN_FILES
        .iter()
        .map(|&(relative, _)| install_dir.join(relative))
        .collect()
}

fn collect_regular_files(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    collect_regular_files_inner(root, &mut out)?;
    Ok(out)
}

fn collect_regular_files_inner(root: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_regular_files_inner(&entry.path(), out)?;
        } else if file_type.is_file() {
            out.push(entry.path());
        }
    }
    Ok(())
}

fn remove_codex_marketplace_entry(home: &Path) -> Result<()> {
    let marketplace_path = codex_personal_marketplace_path(home);
    if !marketplace_path.exists() {
        return Ok(());
    }
    let mut marketplace = load_json_file_strict(&marketplace_path)?;
    let Some(plugins) = marketplace
        .get_mut("plugins")
        .and_then(|value| value.as_array_mut())
    else {
        return Ok(());
    };
    let before = plugins.len();
    plugins.retain(|entry| {
        !matches!(
            entry.get("name").and_then(|value| value.as_str()),
            Some("tracedecay" | "tokensave")
        )
    });
    if plugins.len() == before {
        return Ok(());
    }
    safe_write_json_file(&marketplace_path, &marketplace, None)?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Removed tracedecay from Codex personal marketplace at {}",
        marketplace_path.display()
    );
    Ok(())
}

/// Register MCP server and auto-approve tools in ~/.codex/config.toml.
fn install_mcp_server(config_path: &Path, tracedecay_bin: &str, scope: InstallScope) -> Result<()> {
    let mut config = load_toml_file(config_path)?;

    // Ensure [mcp_servers.tracedecay] exists
    let table = config
        .as_table_mut()
        .ok_or_else(|| TraceDecayError::Config {
            message: "config.toml is not a TOML table".to_string(),
        })?;

    let servers = table
        .entry("mcp_servers")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or_else(|| TraceDecayError::Config {
            message: "mcp_servers is not a table in config.toml".to_string(),
        })?;

    let mut server_table = toml::map::Map::new();
    server_table.insert(
        "command".to_string(),
        toml::Value::String(tracedecay_bin.to_string()),
    );
    let args = match scope {
        InstallScope::Global => vec![toml::Value::String("serve".to_string())],
        InstallScope::ProjectLocal => vec![
            toml::Value::String("serve".to_string()),
            toml::Value::String("--path".to_string()),
            toml::Value::String(".".to_string()),
        ],
    };
    server_table.insert("args".to_string(), toml::Value::Array(args));
    if scope == InstallScope::Global {
        let mut env_table = toml::map::Map::new();
        env_table.insert(
            "TRACEDECAY_ENABLE_GLOBAL_DB".to_string(),
            toml::Value::String("1".to_string()),
        );
        server_table.insert("env".to_string(), toml::Value::Table(env_table));
    }

    // Auto-approve all tracedecay tools so Codex doesn't prompt for each one
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

    servers.insert("tracedecay".to_string(), toml::Value::Table(server_table));

    write_toml_file(config_path, &config)?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Added tracedecay MCP server to {}",
        config_path.display()
    );
    Ok(())
}

/// Append prompt rules to AGENTS.md (idempotent).
fn install_prompt_rules(agents_md: &Path) -> Result<()> {
    let marker = "## Prefer tracedecay MCP tools";
    let existing = if agents_md.exists() {
        std::fs::read_to_string(agents_md).unwrap_or_default()
    } else {
        String::new()
    };
    if existing.contains(marker) {
        eprintln!("  AGENTS.md already contains tracedecay rules, skipping");
        return Ok(());
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(agents_md)
        .map_err(|e| TraceDecayError::Config {
            message: format!("failed to open AGENTS.md: {e}"),
        })?;
    write!(
        f,
        "\n{marker}\n\n\
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
        If you discover a gap where an extractor, schema, or tracedecay tool could be \
        improved to answer a question natively, propose to the user that they open an issue \
        at https://github.com/ScriptedAlchemy/tracedecay describing the limitation. \
        **Remind the user to strip any sensitive or proprietary code from the bug description \
        before submitting.**\n"
    )
    .ok();
    eprintln!(
        "\x1b[32m✔\x1b[0m Appended tracedecay rules to {}",
        agents_md.display()
    );
    Ok(())
}

/// Register tracedecay lifecycle hooks in a Codex `hooks.json` (idempotent).
///
/// Codex organizes hooks as `hooks[event][] -> { matcher?, hooks: [handler] }`
/// where only `type: "command"` handlers run today and `timeout` is in seconds.
/// We register the tracedecay-owned group for each event, preserving any foreign
/// groups, so reinstalls reconcile in place. Backs up an existing file first.
fn install_hooks(hooks_path: &Path, tracedecay_bin: &str) -> Result<()> {
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

    // Steer the agent toward tracedecay MCP tools + report index freshness.
    install_codex_hook_event(
        &mut hooks,
        "SessionStart",
        tracedecay_bin,
        "hook-codex-session-start",
        5,
        None,
    );
    install_codex_hook_event(
        &mut hooks,
        "UserPromptSubmit",
        tracedecay_bin,
        "hook-codex-user-prompt-submit",
        5,
        None,
    );
    install_codex_hook_event(
        &mut hooks,
        "SubagentStart",
        tracedecay_bin,
        "hook-codex-subagent-start",
        5,
        None,
    );
    // Keep the index fresh: apply_patch → targeted single-file sync; Bash →
    // branch-aware / incremental sync. Matcher targets both tool names.
    install_codex_hook_event(
        &mut hooks,
        "PostToolUse",
        tracedecay_bin,
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

/// Insert (or reconcile) the tracedecay-owned matcher group for `event`.
///
/// Drops any pre-existing group that already contains our `subcommand` handler
/// (so refinements to matcher/timeout reach old configs) while preserving every
/// foreign group. Idempotent: exactly one tracedecay group per event.
fn install_codex_hook_event(
    hooks: &mut serde_json::Value,
    event: &str,
    tracedecay_bin: &str,
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
        "command": super::hook_command(tracedecay_bin, subcommand),
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

/// Codex requires non-managed command hooks to be trusted via `/hooks` before
/// they run; newly installed/changed hooks are skipped until trusted.
fn print_hook_trust_guidance() {
    eprintln!();
    eprintln!(
        "\x1b[1mAction required:\x1b[0m Codex skips new/changed command hooks until you trust them."
    );
    eprintln!("  Run \x1b[1m/hooks\x1b[0m inside Codex to review and trust the tracedecay hooks.");
    eprintln!(
        "  (For one-off non-interactive runs you can pass --dangerously-bypass-hook-trust, \
         but trusting via /hooks is recommended.)"
    );
}

// ---------------------------------------------------------------------------
// Uninstall helpers
// ---------------------------------------------------------------------------

/// Remove tracedecay-owned hook groups from a Codex `hooks.json`.
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
            "\x1b[32m✔\x1b[0m Removed tracedecay hooks from {}",
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
    let removed_new = servers.remove("tracedecay").is_some();
    let removed_legacy = servers.remove("tokensave").is_some();
    if !removed_new && !removed_legacy {
        eprintln!(
            "  No tracedecay MCP server in {}, skipping",
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
            "\x1b[32m✔\x1b[0m Removed tracedecay MCP server from {}",
            config_path.display()
        );
    }
    Ok(())
}

/// Remove tracedecay rules from AGENTS.md.
fn uninstall_prompt_rules(agents_md: &Path) {
    if !agents_md.exists() {
        return;
    }
    let Ok(contents) = std::fs::read_to_string(agents_md) else {
        return;
    };
    if !contents.contains("tracedecay") && !contents.contains("tokensave") {
        eprintln!("  AGENTS.md does not contain tracedecay rules, skipping");
        return;
    }
    let marker_new = "## Prefer tracedecay MCP tools";
    let marker_legacy = "## Prefer tokensave MCP tools";
    let (marker, start) = if let Some(start) = contents.find(marker_new) {
        (marker_new, start)
    } else if let Some(start) = contents.find(marker_legacy) {
        (marker_legacy, start)
    } else {
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
            "\x1b[32m✔\x1b[0m Removed tracedecay rules from {}",
            agents_md.display()
        );
    }
}

// ---------------------------------------------------------------------------
// Healthcheck helpers
// ---------------------------------------------------------------------------

fn doctor_check_plugin(dc: &mut DoctorCounters, home: &Path) {
    let plugin_dir = codex_plugin_install_dir(home);
    let manifest_path = plugin_dir.join(".codex-plugin/plugin.json");
    if !manifest_path.exists() {
        if CodexIntegration::has_legacy_config_install(home) {
            doctor_check_config(dc, &home.join(".codex/config.toml"));
            dc.warn(
                "Codex uses a legacy config-managed tracedecay install — run `tracedecay install --agent codex` to install the Codex plugin bundle",
            );
        } else {
            dc.warn(&format!(
                "{} not found — run `tracedecay install --agent codex` if you use Codex CLI",
                manifest_path.display()
            ));
        }
        return;
    }

    let manifest = load_json_file(&manifest_path);
    if manifest.get("name").and_then(|value| value.as_str()) == Some("tracedecay") {
        dc.pass(&format!(
            "Codex plugin manifest present in {}",
            manifest_path.display()
        ));
    } else {
        dc.fail(&format!(
            "Codex plugin manifest at {} is not a tracedecay plugin",
            manifest_path.display()
        ));
    }
    match manifest.get("version").and_then(|value| value.as_str()) {
        Some(env!("CARGO_PKG_VERSION")) => dc.pass("Codex plugin version matches tracedecay"),
        Some(version) => dc.warn(&format!(
            "Codex plugin version {version} does not match tracedecay {} — run `tracedecay update-plugin --agent codex`",
            env!("CARGO_PKG_VERSION")
        )),
        None => dc.warn("Codex plugin manifest does not contain a version"),
    }

    let mcp_path = plugin_dir.join(".mcp.json");
    let mcp = load_json_file(&mcp_path);
    if mcp
        .get("mcpServers")
        .and_then(|servers| servers.get("tracedecay"))
        .is_some()
    {
        dc.pass(&format!(
            "Codex plugin MCP server registered in {}",
            mcp_path.display()
        ));
    } else {
        dc.fail(&format!(
            "Codex plugin MCP server missing in {} — run `tracedecay install --agent codex`",
            mcp_path.display()
        ));
    }

    let marketplace_path = codex_personal_marketplace_path(home);
    let marketplace = load_json_file(&marketplace_path);
    let has_entry = marketplace
        .get("plugins")
        .and_then(|value| value.as_array())
        .is_some_and(|plugins| {
            plugins.iter().any(|entry| {
                entry.get("name").and_then(|value| value.as_str()) == Some("tracedecay")
            })
        });
    if has_entry {
        dc.pass(&format!(
            "Codex personal marketplace contains tracedecay in {}",
            marketplace_path.display()
        ));
    } else {
        dc.warn(&format!(
            "Codex personal marketplace missing tracedecay in {} — run `tracedecay install --agent codex`",
            marketplace_path.display()
        ));
    }
}

/// Check config.toml has tracedecay registered.
fn doctor_check_config(dc: &mut DoctorCounters, config_path: &Path) {
    if !config_path.exists() {
        dc.warn(&format!(
            "{} not found — run `tracedecay install --agent codex` if you use Codex CLI",
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
        .and_then(|v| v.get("tracedecay"))
        .and_then(|v| v.as_table())
        .is_some();

    if !has_server {
        dc.fail(&format!(
            "MCP server NOT registered in {} — run `tracedecay install --agent codex`",
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
        .and_then(|v| v.get("tracedecay"))
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
            "{auto_count}/{tools_len} tools auto-approved — run `tracedecay install --agent codex` to update"
        ));
    } else {
        dc.warn("No tools auto-approved — Codex will prompt for each tool call");
    }
}

/// Check AGENTS.md contains tracedecay rules.
fn doctor_check_prompt_file(dc: &mut DoctorCounters, agents_md: &Path) {
    if agents_md.exists() {
        let has_rules = std::fs::read_to_string(agents_md)
            .unwrap_or_default()
            .contains("tracedecay");
        if has_rules {
            dc.pass(&format!(
                "AGENTS.md contains tracedecay rules in {}",
                agents_md.display()
            ));
        } else {
            dc.fail(&format!(
                "AGENTS.md missing tracedecay rules in {} — run `tracedecay install --local --agent codex` or `tracedecay install --agent codex`",
                agents_md.display()
            ));
        }
    } else {
        dc.warn(&format!("{} does not exist", agents_md.display()));
    }
}

/// Check hooks.json registers the tracedecay lifecycle hooks, and remind the
/// user that Codex requires trusting them via `/hooks` before they run.
fn doctor_check_hooks(dc: &mut DoctorCounters, hooks_path: &Path) {
    if !hooks_path.exists() {
        dc.warn(&format!(
            "{} not found — run `tracedecay install --agent codex` to add lifecycle hooks",
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
            "Codex skips new/changed command hooks until trusted — run `/hooks` in Codex to trust the tracedecay hooks",
        );
    } else {
        dc.warn(&format!(
            "tracedecay hook(s) missing for {} in {} — run `tracedecay install --local --agent codex` or `tracedecay install --agent codex`",
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
