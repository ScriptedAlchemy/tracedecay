//! Cursor agent integration.
//!
//! Installs tokensave's Cursor plugin bundle into Cursor's local plugin
//! directory. The plugin owns MCP, hooks, and rule configuration.

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::errors::{Result, TokenSaveError};

use super::{
    backup_and_write_json, load_json_file, load_jsonc_file_strict, AgentIntegration,
    DoctorCounters, HealthcheckContext, InstallContext,
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
        install_cursor_plugin(&ctx.home)?;

        eprintln!();
        eprintln!("Setup complete. Next steps:");
        eprintln!("  1. cd into your project and run: tokensave init");
        eprintln!("  2. Reload Cursor — the tokensave plugin is now installed");
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
            cursor_dir.join("hooks.json"),
        ] {
            super::ensure_project_local_safe_path(project_path, &path)?;
        }
        install_cursor_plugin(&ctx.home)?;
        remove_legacy_project_mcp(&cursor_dir.join("mcp.json"));
        remove_legacy_project_hooks(&cursor_dir.join("hooks.json"))?;
        remove_legacy_project_rule(&cursor_dir.join("rules/tokensave.mdc"))?;

        eprintln!();
        eprintln!("Cursor local setup uses the tokensave Cursor plugin.");
        eprintln!("Reload Cursor so the plugin loads for this workspace.");
        Ok(())
    }

    fn uninstall(&self, ctx: &InstallContext) -> Result<()> {
        remove_cursor_plugin_install(&cursor_plugin_install_dir(&ctx.home))?;
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
        doctor_check_plugin(dc, &ctx.home);
        if project_cursor.join("mcp.json").exists()
            || project_cursor.join("hooks.json").exists()
            || project_cursor.join("rules/tokensave.mdc").exists()
        {
            dc.warn(
                "legacy project Cursor MCP/hooks/rule files are present; rerun \
                 `tokensave install --local --agent cursor` to remove tokensave-owned entries",
            );
        }
    }

    fn is_detected(&self, home: &Path) -> bool {
        home.join(".cursor").is_dir()
    }

    fn primary_config_path(&self, home: &Path) -> Option<std::path::PathBuf> {
        Some(cursor_plugin_manifest_path(home))
    }

    fn has_tokensave(&self, home: &Path) -> bool {
        cursor_plugin_manifest_path(home).exists()
            || legacy_mcp_has_tokensave(&home.join(".cursor/mcp.json"))
    }
}

// ---------------------------------------------------------------------------
// Plugin install helpers
// ---------------------------------------------------------------------------

const PLUGIN_MANIFEST: &str = include_str!("../../cursor-plugin/.cursor-plugin/plugin.json");
const PLUGIN_MCP: &str = include_str!("../../cursor-plugin/mcp.json");
const PLUGIN_HOOKS: &str = include_str!("../../cursor-plugin/hooks/hooks.json");
const PLUGIN_RULE: &str = include_str!("../../cursor-plugin/rules/tokensave.mdc");

fn cursor_plugin_install_dir(home: &Path) -> PathBuf {
    home.join(".cursor/plugins/local/tokensave")
}

fn cursor_plugin_manifest_path(home: &Path) -> PathBuf {
    cursor_plugin_install_dir(home).join(".cursor-plugin/plugin.json")
}

fn cursor_plugin_source_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("cursor-plugin")
}

fn install_cursor_plugin(home: &Path) -> Result<()> {
    let install_dir = cursor_plugin_install_dir(home);
    if let Some(parent) = install_dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| TokenSaveError::Config {
            message: format!("failed to create {}: {e}", parent.display()),
        })?;
    }
    remove_cursor_plugin_install(&install_dir)?;

    let source = cursor_plugin_source_dir();
    if source.join(".cursor-plugin/plugin.json").exists()
        && symlink_plugin_dir(&source, &install_dir).is_ok()
    {
        eprintln!(
            "\x1b[32m✔\x1b[0m Installed Cursor plugin symlink at {}",
            install_dir.display()
        );
        return Ok(());
    }

    write_embedded_plugin(&install_dir)?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Installed Cursor plugin at {}",
        install_dir.display()
    );
    Ok(())
}

#[cfg(unix)]
fn symlink_plugin_dir(source: &Path, install_dir: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(source, install_dir)
}

#[cfg(not(unix))]
fn symlink_plugin_dir(_source: &Path, _install_dir: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "directory symlink is not supported on this platform",
    ))
}

fn write_embedded_plugin(install_dir: &Path) -> Result<()> {
    write_generated_text(
        &install_dir.join(".cursor-plugin/plugin.json"),
        PLUGIN_MANIFEST,
    )?;
    write_generated_text(&install_dir.join("mcp.json"), PLUGIN_MCP)?;
    write_generated_text(&install_dir.join("hooks/hooks.json"), PLUGIN_HOOKS)?;
    write_generated_text(&install_dir.join("rules/tokensave.mdc"), PLUGIN_RULE)
}

fn remove_cursor_plugin_install(install_dir: &Path) -> Result<()> {
    let Ok(metadata) = std::fs::symlink_metadata(install_dir) else {
        return Ok(());
    };
    if metadata.file_type().is_symlink() || metadata.is_file() {
        std::fs::remove_file(install_dir).map_err(|e| TokenSaveError::Config {
            message: format!("failed to remove {}: {e}", install_dir.display()),
        })?;
        return Ok(());
    }
    if !metadata.is_dir() {
        return Err(TokenSaveError::Config {
            message: format!(
                "refusing to replace non-directory Cursor plugin path {}",
                install_dir.display()
            ),
        });
    }
    if !cursor_plugin_dir_is_tokensave(install_dir) {
        return Err(TokenSaveError::Config {
            message: format!(
                "refusing to replace unmanaged Cursor plugin directory {}",
                install_dir.display()
            ),
        });
    }
    if cursor_plugin_dir_has_only_managed_files(install_dir) {
        std::fs::remove_dir_all(install_dir).map_err(|e| TokenSaveError::Config {
            message: format!("failed to remove {}: {e}", install_dir.display()),
        })?;
    } else {
        for path in cursor_plugin_managed_paths(install_dir) {
            if path.exists() {
                std::fs::remove_file(&path).ok();
            }
        }
    }
    Ok(())
}

fn cursor_plugin_dir_is_tokensave(install_dir: &Path) -> bool {
    let manifest = load_json_file(&install_dir.join(".cursor-plugin/plugin.json"));
    manifest.get("name").and_then(|v| v.as_str()) == Some("tokensave")
}

fn cursor_plugin_dir_has_only_managed_files(install_dir: &Path) -> bool {
    let Ok(entries) = collect_regular_files(install_dir) else {
        return false;
    };
    let managed = cursor_plugin_managed_paths(install_dir);
    entries
        .iter()
        .all(|entry| managed.iter().any(|managed| managed == entry))
}

fn cursor_plugin_managed_paths(install_dir: &Path) -> Vec<PathBuf> {
    [
        ".cursor-plugin/plugin.json",
        "mcp.json",
        "hooks/hooks.json",
        "rules/tokensave.mdc",
        "README.md",
    ]
    .into_iter()
    .map(|relative| install_dir.join(relative))
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
        let path = entry.path();
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            collect_regular_files_inner(&path, out)?;
        } else if metadata.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

fn legacy_mcp_has_tokensave(mcp_path: &Path) -> bool {
    load_json_file(mcp_path)
        .get("mcpServers")
        .and_then(|v| v.get("tokensave"))
        .is_some()
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

fn remove_legacy_project_mcp(mcp_path: &Path) {
    uninstall_mcp_server(mcp_path);
}

fn remove_legacy_project_hooks(hooks_path: &Path) -> Result<()> {
    if !hooks_path.exists() {
        return Ok(());
    }
    let mut hooks = load_jsonc_file_strict(hooks_path)?;
    let Some(events) = hooks
        .get_mut("hooks")
        .and_then(|value| value.as_object_mut())
    else {
        return Ok(());
    };

    let mut removed = false;
    let event_names: Vec<String> = events.keys().cloned().collect();
    for event in event_names {
        let Some(entries) = events
            .get_mut(&event)
            .and_then(|value| value.as_array_mut())
        else {
            continue;
        };
        let before = entries.len();
        entries.retain(|entry| {
            !entry
                .get("command")
                .and_then(|value| value.as_str())
                .is_some_and(|command| command.contains("hook-cursor-"))
        });
        removed |= entries.len() != before;
    }
    events.retain(|_, value| value.as_array().is_none_or(|entries| !entries.is_empty()));

    if !removed {
        return Ok(());
    }
    if events.is_empty() {
        std::fs::remove_file(hooks_path).map_err(|e| TokenSaveError::Config {
            message: format!("failed to remove {}: {e}", hooks_path.display()),
        })?;
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed legacy Cursor hooks from {}",
            hooks_path.display()
        );
    } else if backup_and_write_json(hooks_path, &hooks) {
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed legacy tokensave hooks from {}",
            hooks_path.display()
        );
    }
    Ok(())
}

fn remove_legacy_project_rule(rule_path: &Path) -> Result<()> {
    if !rule_path.exists() {
        return Ok(());
    }
    let contents = std::fs::read_to_string(rule_path).map_err(|e| TokenSaveError::Config {
        message: format!("failed to read {}: {e}", rule_path.display()),
    })?;
    if contents.contains("tokensave MCP tools") {
        std::fs::remove_file(rule_path).map_err(|e| TokenSaveError::Config {
            message: format!("failed to remove {}: {e}", rule_path.display()),
        })?;
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed legacy Cursor rule from {}",
            rule_path.display()
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Healthcheck helpers
// ---------------------------------------------------------------------------

fn doctor_check_plugin(dc: &mut DoctorCounters, home: &Path) {
    let plugin_dir = cursor_plugin_install_dir(home);
    let manifest_path = plugin_dir.join(".cursor-plugin/plugin.json");
    if !manifest_path.exists() {
        dc.warn(&format!(
            "{} not found — run `tokensave install --agent cursor` if you use Cursor",
            manifest_path.display()
        ));
        if legacy_mcp_has_tokensave(&home.join(".cursor/mcp.json")) {
            dc.warn(
                "legacy Cursor MCP config is installed; rerun install to use the Cursor plugin",
            );
        }
        return;
    }

    let manifest = load_json_file(&manifest_path);
    if manifest.get("name").and_then(|v| v.as_str()) == Some("tokensave")
        && manifest.get("mcpServers").and_then(|v| v.as_str()) == Some("mcp.json")
        && manifest.get("hooks").and_then(|v| v.as_str()) == Some("hooks/hooks.json")
    {
        dc.pass(&format!(
            "Cursor plugin manifest active in {}",
            manifest_path.display()
        ));
    } else {
        dc.fail(&format!(
            "Cursor tokensave plugin manifest is incomplete in {}",
            manifest_path.display()
        ));
    }
    doctor_check_plugin_mcp(dc, &plugin_dir.join("mcp.json"));
    doctor_check_plugin_hooks(dc, &plugin_dir.join("hooks/hooks.json"));
    doctor_check_plugin_rule(dc, &plugin_dir.join("rules/tokensave.mdc"));
}

fn doctor_check_plugin_mcp(dc: &mut DoctorCounters, mcp_path: &Path) {
    let settings = load_json_file(mcp_path);
    let server = &settings["mcpServers"]["tokensave"];
    if server["command"] == "tokensave"
        && server["args"] == json!(["serve", "--path", "${workspaceFolder}"])
    {
        dc.pass(&format!(
            "Cursor plugin MCP registered in {}",
            mcp_path.display()
        ));
    } else {
        dc.fail(&format!(
            "Cursor plugin MCP config is incomplete in {} — run `tokensave install --agent cursor`",
            mcp_path.display()
        ));
    }
}

fn doctor_check_plugin_hooks(dc: &mut DoctorCounters, hooks_path: &Path) {
    if !hooks_path.exists() {
        dc.warn(&format!(
            "{} not found — run `tokensave install --agent cursor`",
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
            "All {} Cursor plugin lifecycle hooks registered in {}",
            expected.len(),
            hooks_path.display()
        ));
    } else {
        dc.fail(&format!(
            "Cursor plugin hook(s) missing for {} — run `tokensave install --agent cursor`",
            missing.join(", ")
        ));
    }
}

fn doctor_check_plugin_rule(dc: &mut DoctorCounters, rule_path: &Path) {
    if !rule_path.exists() {
        dc.warn(&format!(
            "{} not found — run `tokensave install --agent cursor`",
            rule_path.display()
        ));
        return;
    }
    let contents = std::fs::read_to_string(rule_path).unwrap_or_default();
    if contents.contains("alwaysApply: true") && contents.contains("tokensave MCP tools") {
        dc.pass(&format!(
            "Cursor plugin tokensave rule active in {}",
            rule_path.display()
        ));
    } else {
        dc.fail(&format!(
            "Cursor plugin tokensave rule is incomplete in {} — run `tokensave install --agent cursor`",
            rule_path.display()
        ));
    }
}
