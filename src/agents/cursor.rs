//! Cursor agent integration.
//!
//! Installs tokensave's Cursor plugin bundle into Cursor's local plugin
//! directory. The plugin owns MCP, hooks, and rule configuration.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use serde_json::json;

use crate::errors::{Result, TokenSaveError};

use super::{
    backup_and_write_json, load_json_file, load_jsonc_file_strict, safe_write_text_file,
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
        install_cursor_plugin(&ctx.home, &ctx.tokensave_bin)?;

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
        install_cursor_plugin(&ctx.home, &ctx.tokensave_bin)?;
        uninstall_mcp_server(&cursor_dir.join("mcp.json"));
        remove_legacy_project_hooks(&cursor_dir.join("hooks.json"))?;
        remove_legacy_project_rule(&cursor_dir.join("rules/tokensave.mdc"))?;

        eprintln!();
        eprintln!("Cursor local setup uses the tokensave Cursor plugin.");
        eprintln!("Reload Cursor so the plugin loads for this workspace.");
        Ok(())
    }

    fn post_install<'a>(
        &'a self,
        project_path: Option<&'a Path>,
    ) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
        Box::pin(track_branch_after_install(project_path))
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
        if legacy_project_cursor_has_tokensave(&project_cursor) {
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
// Post-install hook
// ---------------------------------------------------------------------------

/// Registers the project's current git branch for tokensave indexing after a
/// Cursor plugin install, so per-branch graphs stay in sync from the moment
/// the integration is set up.
///
/// No-ops when there is no project path, no branch can be resolved, or the
/// project has not been indexed yet (so it never bootstraps an index on its
/// own).
async fn track_branch_after_install(project_path: Option<&Path>) {
    let Some(project_path) = project_path else {
        return;
    };
    let Some(branch_name) = crate::branch::current_branch(project_path) else {
        return;
    };
    match crate::branch::add_branch_tracking(project_path, &branch_name).await {
        Ok(crate::branch::BranchAddOutcome::Added) => {
            eprintln!(
                "\x1b[32m✔\x1b[0m Tracked Cursor branch '{branch_name}' for tokensave indexing"
            );
        }
        Ok(
            crate::branch::BranchAddOutcome::AlreadyTracked
            | crate::branch::BranchAddOutcome::NotIndexed,
        ) => {}
        Err(err) => {
            eprintln!(
                "\x1b[33mwarning:\x1b[0m could not track Cursor branch '{branch_name}' for tokensave indexing: {err}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Plugin install helpers
// ---------------------------------------------------------------------------

/// Every file in the Cursor plugin bundle, embedded into the binary so installs
/// work from a released binary without the repo `cursor-plugin/` source tree.
/// Each entry is `(relative_path, file_contents)`. This is the single source of
/// truth for the embedded writer, the managed-path set used for uninstall, and
/// the coverage-guard test. The manifest, `mcp.json`, and `hooks/hooks.json`
/// entries are rendered through helpers at install time to inject the package
/// version and the absolute tokensave binary path.
const EMBEDDED_PLUGIN_FILES: &[(&str, &str)] = &[
    (
        ".cursor-plugin/plugin.json",
        include_str!("../../cursor-plugin/.cursor-plugin/plugin.json"),
    ),
    ("README.md", include_str!("../../cursor-plugin/README.md")),
    ("mcp.json", include_str!("../../cursor-plugin/mcp.json")),
    (
        "hooks/hooks.json",
        include_str!("../../cursor-plugin/hooks/hooks.json"),
    ),
    (
        "rules/tokensave.mdc",
        include_str!("../../cursor-plugin/rules/tokensave.mdc"),
    ),
    (
        "skills/architecture-overview/SKILL.md",
        include_str!("../../cursor-plugin/skills/architecture-overview/SKILL.md"),
    ),
    (
        "skills/atomic-code-edits/SKILL.md",
        include_str!("../../cursor-plugin/skills/atomic-code-edits/SKILL.md"),
    ),
    (
        "skills/cleaning-up-dead-code/SKILL.md",
        include_str!("../../cursor-plugin/skills/cleaning-up-dead-code/SKILL.md"),
    ),
    (
        "skills/code-health-report/SKILL.md",
        include_str!("../../cursor-plugin/skills/code-health-report/SKILL.md"),
    ),
    (
        "skills/cross-branch-investigation/SKILL.md",
        include_str!("../../cursor-plugin/skills/cross-branch-investigation/SKILL.md"),
    ),
    (
        "skills/drafting-commit-and-pr/SKILL.md",
        include_str!("../../cursor-plugin/skills/drafting-commit-and-pr/SKILL.md"),
    ),
    (
        "skills/finding-impacted-areas/SKILL.md",
        include_str!("../../cursor-plugin/skills/finding-impacted-areas/SKILL.md"),
    ),
    (
        "skills/fixing-build-and-type-errors/SKILL.md",
        include_str!("../../cursor-plugin/skills/fixing-build-and-type-errors/SKILL.md"),
    ),
    (
        "skills/memorizing-subject/SKILL.md",
        include_str!("../../cursor-plugin/skills/memorizing-subject/SKILL.md"),
    ),
    (
        "skills/porting-code/SKILL.md",
        include_str!("../../cursor-plugin/skills/porting-code/SKILL.md"),
    ),
    (
        "skills/project-status/SKILL.md",
        include_str!("../../cursor-plugin/skills/project-status/SKILL.md"),
    ),
    (
        "skills/recalling-project-memory/SKILL.md",
        include_str!("../../cursor-plugin/skills/recalling-project-memory/SKILL.md"),
    ),
    (
        "skills/reviewing-a-diff/SKILL.md",
        include_str!("../../cursor-plugin/skills/reviewing-a-diff/SKILL.md"),
    ),
    (
        "skills/running-impacted-tests/SKILL.md",
        include_str!("../../cursor-plugin/skills/running-impacted-tests/SKILL.md"),
    ),
    (
        "skills/searching-for-code/SKILL.md",
        include_str!("../../cursor-plugin/skills/searching-for-code/SKILL.md"),
    ),
    (
        "skills/tracing-functions/SKILL.md",
        include_str!("../../cursor-plugin/skills/tracing-functions/SKILL.md"),
    ),
    (
        "agents/code-explorer.md",
        include_str!("../../cursor-plugin/agents/code-explorer.md"),
    ),
    (
        "agents/code-health-auditor.md",
        include_str!("../../cursor-plugin/agents/code-health-auditor.md"),
    ),
    (
        "commands/memorize-subject.md",
        include_str!("../../cursor-plugin/commands/memorize-subject.md"),
    ),
    (
        "commands/tokensave-arch.md",
        include_str!("../../cursor-plugin/commands/tokensave-arch.md"),
    ),
    (
        "commands/tokensave-branch.md",
        include_str!("../../cursor-plugin/commands/tokensave-branch.md"),
    ),
    (
        "commands/tokensave-diagnose.md",
        include_str!("../../cursor-plugin/commands/tokensave-diagnose.md"),
    ),
    (
        "commands/tokensave-health.md",
        include_str!("../../cursor-plugin/commands/tokensave-health.md"),
    ),
    (
        "commands/tokensave-port.md",
        include_str!("../../cursor-plugin/commands/tokensave-port.md"),
    ),
    (
        "commands/tokensave-review.md",
        include_str!("../../cursor-plugin/commands/tokensave-review.md"),
    ),
];

fn cursor_plugin_install_dir(home: &Path) -> PathBuf {
    home.join(".cursor/plugins/local/tokensave")
}

fn cursor_plugin_manifest_path(home: &Path) -> PathBuf {
    cursor_plugin_install_dir(home).join(".cursor-plugin/plugin.json")
}

fn install_cursor_plugin(home: &Path, tokensave_bin: &str) -> Result<()> {
    let install_dir = cursor_plugin_install_dir(home);
    if let Some(parent) = install_dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| TokenSaveError::Config {
            message: format!("failed to create {}: {e}", parent.display()),
        })?;
    }
    remove_cursor_plugin_install(&install_dir)?;

    write_embedded_plugin(&install_dir, tokensave_bin)?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Installed Cursor plugin at {}",
        install_dir.display()
    );
    Ok(())
}

fn write_embedded_plugin(install_dir: &Path, tokensave_bin: &str) -> Result<()> {
    for &(relative, contents) in EMBEDDED_PLUGIN_FILES {
        let rendered = match relative {
            ".cursor-plugin/plugin.json" => cursor_plugin_manifest(contents)?,
            "mcp.json" => cursor_plugin_mcp(contents, tokensave_bin)?,
            "hooks/hooks.json" => cursor_plugin_hooks(contents, tokensave_bin)?,
            _ => contents.to_string(),
        };
        safe_write_text_file(&install_dir.join(relative), &rendered, None)?;
    }
    Ok(())
}

fn cursor_plugin_manifest(raw: &str) -> Result<String> {
    let mut manifest: serde_json::Value = serde_json::from_str(raw)?;
    manifest["version"] = json!(env!("CARGO_PKG_VERSION"));
    Ok(format!("{}\n", serde_json::to_string_pretty(&manifest)?))
}

fn cursor_plugin_mcp(raw: &str, tokensave_bin: &str) -> Result<String> {
    let mut mcp: serde_json::Value = serde_json::from_str(raw)?;
    mcp["mcpServers"]["tokensave"]["command"] = json!(tokensave_bin);
    Ok(format!("{}\n", serde_json::to_string_pretty(&mcp)?))
}

fn cursor_plugin_hooks(raw: &str, tokensave_bin: &str) -> Result<String> {
    let mut hooks: serde_json::Value = serde_json::from_str(raw)?;
    if let Some(events) = hooks
        .get_mut("hooks")
        .and_then(|value| value.as_object_mut())
    {
        for entries in events.values_mut().filter_map(|value| value.as_array_mut()) {
            for entry in entries {
                if let Some(command_value) = entry.get_mut("command") {
                    let Some(command) = command_value.as_str() else {
                        continue;
                    };
                    if let Some(suffix) = command.strip_prefix("tokensave ") {
                        *command_value = json!(format!("{tokensave_bin} {suffix}"));
                    }
                }
            }
        }
    }
    Ok(format!("{}\n", serde_json::to_string_pretty(&hooks)?))
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
            std::fs::remove_file(&path).ok();
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
    entries.iter().all(|entry| managed.contains(entry))
}

fn cursor_plugin_managed_paths(install_dir: &Path) -> Vec<PathBuf> {
    EMBEDDED_PLUGIN_FILES
        .iter()
        .map(|&(relative, _)| relative)
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
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_regular_files_inner(&entry.path(), out)?;
        } else if file_type.is_file() {
            out.push(entry.path());
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

fn legacy_project_cursor_has_tokensave(cursor_dir: &Path) -> bool {
    legacy_mcp_has_tokensave(&cursor_dir.join("mcp.json"))
        || legacy_hooks_have_tokensave(&cursor_dir.join("hooks.json"))
        || legacy_rule_has_tokensave(&cursor_dir.join("rules/tokensave.mdc"))
}

/// A Cursor hook entry is tokensave-owned when its `command` runs a
/// `hook-cursor-*` subcommand.
fn is_legacy_tokensave_hook(entry: &serde_json::Value) -> bool {
    entry
        .get("command")
        .and_then(|value| value.as_str())
        .is_some_and(|command| command.contains("hook-cursor-"))
}

fn legacy_hooks_have_tokensave(hooks_path: &Path) -> bool {
    load_json_file(hooks_path)
        .get("hooks")
        .and_then(|value| value.as_object())
        .is_some_and(|events| {
            events.values().any(|value| {
                value
                    .as_array()
                    .is_some_and(|entries| entries.iter().any(is_legacy_tokensave_hook))
            })
        })
}

fn legacy_rule_has_tokensave(rule_path: &Path) -> bool {
    std::fs::read_to_string(rule_path)
        .is_ok_and(|contents| contents.contains("tokensave MCP tools"))
}

/// Remove the tokensave MCP server entry from a Cursor `mcp.json`, deleting the
/// file when it becomes empty and otherwise backing up before rewriting.
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
    for value in events.values_mut() {
        let Some(entries) = value.as_array_mut() else {
            continue;
        };
        let before = entries.len();
        entries.retain(|entry| !is_legacy_tokensave_hook(entry));
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
    let manifest_path = cursor_plugin_manifest_path(home);
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
    if !mcp_path.exists() {
        dc.warn(&format!(
            "{} not found — run `tokensave install --agent cursor`",
            mcp_path.display()
        ));
        return;
    }
    let settings = load_json_file(mcp_path);
    let server = &settings["mcpServers"]["tokensave"];
    if server["command"]
        .as_str()
        .is_some_and(|command| !command.is_empty())
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn cursor_plugin_source_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("cursor-plugin")
    }

    fn relative_paths_under(root: &Path) -> Vec<String> {
        let mut paths: Vec<String> = collect_regular_files(root)
            .expect("source bundle should be readable")
            .iter()
            .map(|path| {
                path.strip_prefix(root)
                    .expect("collected paths live under root")
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        paths.sort();
        paths
    }

    #[test]
    fn write_embedded_plugin_writes_core_and_bundle_files() {
        let tmp = TempDir::new().unwrap();
        let install_dir = tmp.path().join("tokensave");
        write_embedded_plugin(&install_dir, "tokensave").expect("embedded install should succeed");

        // The four core files land, and the manifest is valid JSON carrying the
        // mcpServers key released binaries rely on.
        let manifest_path = install_dir.join(".cursor-plugin/plugin.json");
        let manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
        assert_eq!(manifest["name"], "tokensave");
        assert_eq!(manifest["mcpServers"], "mcp.json");
        assert!(install_dir.join("README.md").exists());
        assert!(install_dir.join("mcp.json").exists());
        assert!(install_dir.join("hooks/hooks.json").exists());
        assert!(install_dir.join("rules/tokensave.mdc").exists());

        // A representative skill, the agent, and a command also ship, so released
        // installs are no longer missing the bundle that the symlink path provides.
        assert!(
            install_dir
                .join("skills/searching-for-code/SKILL.md")
                .exists(),
            "a representative skill should be embedded"
        );
        assert!(
            install_dir.join("agents/code-explorer.md").exists(),
            "the code-explorer agent should be embedded"
        );
        assert!(
            install_dir.join("commands/tokensave-arch.md").exists(),
            "a representative command should be embedded"
        );

        // Every embedded file is also a managed path so uninstall can clean it.
        let managed = cursor_plugin_managed_paths(&install_dir);
        for &(relative, _) in EMBEDDED_PLUGIN_FILES {
            assert!(
                managed.contains(&install_dir.join(relative)),
                "{relative} should be a managed path"
            );
        }
    }

    #[test]
    fn embedded_file_list_covers_the_whole_source_bundle() {
        let on_disk = relative_paths_under(&cursor_plugin_source_dir());
        let mut expected: Vec<String> = EMBEDDED_PLUGIN_FILES
            .iter()
            .map(|&(relative, _)| relative.to_string())
            .collect();
        expected.sort();
        assert_eq!(
            on_disk, expected,
            "EMBEDDED_PLUGIN_FILES must cover every cursor-plugin file"
        );
    }

    #[test]
    fn embedded_install_uninstalls_completely() {
        let tmp = TempDir::new().unwrap();
        let install_dir = tmp.path().join("tokensave");
        write_embedded_plugin(&install_dir, "tokensave").expect("embedded install should succeed");
        assert!(install_dir
            .join("skills/searching-for-code/SKILL.md")
            .exists());

        // Because managed paths cover every embedded file, uninstall recognises a
        // tokensave-only directory and removes it entirely.
        remove_cursor_plugin_install(&install_dir).expect("uninstall should succeed");
        assert!(
            !install_dir.exists(),
            "embedded install should be fully removed on uninstall"
        );
    }

    /// The Cursor `post_install` hook (the branch-tracking logic that moved
    /// off `main` and onto the integration) must be safe to run on a project
    /// tokensave has not indexed: it must not bootstrap a `.tokensave/` index
    /// or panic.
    #[tokio::test]
    async fn post_install_does_not_bootstrap_index() {
        let project = tempfile::tempdir().expect("tempdir");
        CursorIntegration.post_install(Some(project.path())).await;
        assert!(
            !project.path().join(".tokensave").exists(),
            "post_install must not create an index on an unindexed project"
        );
    }

    /// A `None` project path is a no-op and must not panic.
    #[tokio::test]
    async fn post_install_handles_missing_project_path() {
        CursorIntegration.post_install(None).await;
    }
}
