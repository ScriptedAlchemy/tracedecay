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
    AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext, UpdatePluginOutcome,
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
        sweep_legacy_project_artifacts_at_cwd(&ctx.home);

        eprintln!();
        eprintln!("Setup complete. Next steps:");
        eprintln!("  1. cd into your project and run: tokensave init");
        eprintln!("  2. Reload Cursor — the tokensave plugin is now installed");
        eprintln!(
            "  3. Optional: Cursor's Auto-review mode reviews every MCP call; to let \
             tokensave's read-only tools run without per-call review, copy the \
             permissions.json mcpAllowlist snippet from the plugin README \
             ({})",
            cursor_plugin_install_dir(&ctx.home)
                .join("README.md")
                .display()
        );
        Ok(())
    }

    fn supports_local_install(&self) -> bool {
        true
    }

    fn install_local(&self, ctx: &InstallContext, project_path: &Path) -> Result<()> {
        install_cursor_plugin(&ctx.home, &ctx.tokensave_bin)?;
        sweep_legacy_project_artifacts(project_path)?;

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

    fn update_plugin(&self, ctx: &InstallContext) -> Result<UpdatePluginOutcome> {
        // The whole plugin directory is a tokensave-generated bundle (its
        // mcp.json / hooks.json are rendered artifacts, not user config), so
        // refreshing it is exactly the install path. User config such as
        // `~/.cursor/mcp.json` is never written by `install_cursor_plugin`,
        // and unmanaged files inside the plugin dir are preserved.
        if !cursor_plugin_manifest_path(&ctx.home).exists() {
            return Ok(UpdatePluginOutcome::NotInstalled);
        }
        install_cursor_plugin(&ctx.home, &ctx.tokensave_bin)?;
        sweep_legacy_project_artifacts_at_cwd(&ctx.home);
        Ok(UpdatePluginOutcome::Refreshed(vec![
            cursor_plugin_install_dir(&ctx.home),
        ]))
    }

    fn uninstall(&self, ctx: &InstallContext) -> Result<()> {
        remove_cursor_plugin_install(&cursor_plugin_install_dir(&ctx.home))?;
        let mcp_path = ctx.home.join(".cursor/mcp.json");
        uninstall_mcp_server(&mcp_path);
        sweep_legacy_project_artifacts_at_cwd(&ctx.home);

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
                 `tokensave install --agent cursor` from this project to remove \
                 tokensave-owned entries",
            );
        }
        doctor_check_session_ingest(dc, &ctx.project_path);
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
        "skills/assessing-test-coverage/SKILL.md",
        include_str!("../../cursor-plugin/skills/assessing-test-coverage/SKILL.md"),
    ),
    (
        "skills/atomic-code-edits/SKILL.md",
        include_str!("../../cursor-plugin/skills/atomic-code-edits/SKILL.md"),
    ),
    (
        "skills/auditing-code-safety/SKILL.md",
        include_str!("../../cursor-plugin/skills/auditing-code-safety/SKILL.md"),
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
        "skills/curating-project-memory/SKILL.md",
        include_str!("../../cursor-plugin/skills/curating-project-memory/SKILL.md"),
    ),
    (
        "skills/drafting-commit-and-pr/SKILL.md",
        include_str!("../../cursor-plugin/skills/drafting-commit-and-pr/SKILL.md"),
    ),
    (
        "skills/exploring-types-and-traits/SKILL.md",
        include_str!("../../cursor-plugin/skills/exploring-types-and-traits/SKILL.md"),
    ),
    (
        "skills/finding-duplicate-logic/SKILL.md",
        include_str!("../../cursor-plugin/skills/finding-duplicate-logic/SKILL.md"),
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
        "skills/memorize-subject/SKILL.md",
        include_str!("../../cursor-plugin/skills/memorize-subject/SKILL.md"),
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
        "skills/reading-code-cheaply/SKILL.md",
        include_str!("../../cursor-plugin/skills/reading-code-cheaply/SKILL.md"),
    ),
    (
        "skills/recalling-project-memory/SKILL.md",
        include_str!("../../cursor-plugin/skills/recalling-project-memory/SKILL.md"),
    ),
    (
        "skills/recalling-session-context/SKILL.md",
        include_str!("../../cursor-plugin/skills/recalling-session-context/SKILL.md"),
    ),
    (
        "skills/refactoring-safely/SKILL.md",
        include_str!("../../cursor-plugin/skills/refactoring-safely/SKILL.md"),
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
    // Slash-command dispatcher skills (`disable-model-invocation: true`).
    // Slugs keep the `tokensave-` prefix (so `/tokensave` lists them all) with
    // a verb-phrase suffix, because Cursor uses the humanized slug as the
    // skill's display title.
    (
        "skills/tokensave-audit-safety/SKILL.md",
        include_str!("../../cursor-plugin/skills/tokensave-audit-safety/SKILL.md"),
    ),
    (
        "skills/tokensave-check-health/SKILL.md",
        include_str!("../../cursor-plugin/skills/tokensave-check-health/SKILL.md"),
    ),
    (
        "skills/tokensave-clean-dead-code/SKILL.md",
        include_str!("../../cursor-plugin/skills/tokensave-clean-dead-code/SKILL.md"),
    ),
    (
        "skills/tokensave-compare-branches/SKILL.md",
        include_str!("../../cursor-plugin/skills/tokensave-compare-branches/SKILL.md"),
    ),
    (
        "skills/tokensave-curate-memory/SKILL.md",
        include_str!("../../cursor-plugin/skills/tokensave-curate-memory/SKILL.md"),
    ),
    (
        "skills/tokensave-draft-commit/SKILL.md",
        include_str!("../../cursor-plugin/skills/tokensave-draft-commit/SKILL.md"),
    ),
    (
        "skills/tokensave-find-impact/SKILL.md",
        include_str!("../../cursor-plugin/skills/tokensave-find-impact/SKILL.md"),
    ),
    (
        "skills/tokensave-fix-build/SKILL.md",
        include_str!("../../cursor-plugin/skills/tokensave-fix-build/SKILL.md"),
    ),
    (
        "skills/tokensave-map-architecture/SKILL.md",
        include_str!("../../cursor-plugin/skills/tokensave-map-architecture/SKILL.md"),
    ),
    (
        "skills/tokensave-port-code/SKILL.md",
        include_str!("../../cursor-plugin/skills/tokensave-port-code/SKILL.md"),
    ),
    (
        "skills/tokensave-recall-memory/SKILL.md",
        include_str!("../../cursor-plugin/skills/tokensave-recall-memory/SKILL.md"),
    ),
    (
        "skills/tokensave-review-diff/SKILL.md",
        include_str!("../../cursor-plugin/skills/tokensave-review-diff/SKILL.md"),
    ),
    (
        "skills/tokensave-test-changes/SKILL.md",
        include_str!("../../cursor-plugin/skills/tokensave-test-changes/SKILL.md"),
    ),
    (
        "skills/tracing-functions/SKILL.md",
        include_str!("../../cursor-plugin/skills/tracing-functions/SKILL.md"),
    ),
    (
        "skills/tracking-session-health/SKILL.md",
        include_str!("../../cursor-plugin/skills/tracking-session-health/SKILL.md"),
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
        "agents/session-historian.md",
        include_str!("../../cursor-plugin/agents/session-historian.md"),
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

/// Bundle directories shipped by older tokensave plugin versions that no
/// longer exist in the current bundle. Swept during replace/uninstall so
/// upgrades don't strand stale surfaces (managed-path removal only covers
/// files the *current* bundle ships). `commands/` was migrated to slash
/// skills (`disable-model-invocation: true`) when Cursor deprecated the
/// standalone Commands surface; the `skills/tokensave-*` entries are the
/// pre-rename dispatcher slugs (renamed to verb-phrase slugs because Cursor
/// displays the humanized slug as the skill title).
const LEGACY_PLUGIN_DIRS: &[&str] = &[
    "commands",
    "skills/tokensave-arch",
    "skills/tokensave-audit",
    "skills/tokensave-branch",
    "skills/tokensave-clean",
    "skills/tokensave-commit",
    "skills/tokensave-diagnose",
    "skills/tokensave-health",
    "skills/tokensave-impact",
    "skills/tokensave-port",
    "skills/tokensave-recall",
    "skills/tokensave-review",
    "skills/tokensave-test",
];

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
    // The directory is tokensave-owned: sweep bundle dirs that older versions
    // shipped, so they don't count as "unmanaged" leftovers below and linger
    // across upgrades.
    for legacy in LEGACY_PLUGIN_DIRS {
        let path = install_dir.join(legacy);
        if path.is_dir() {
            std::fs::remove_dir_all(&path).ok();
        }
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

/// Removes legacy PROJECT-local tokensave artifacts. Pre-plugin versions of
/// `tokensave install --local` wrote the MCP server entry, lifecycle hooks,
/// and the steering rule into `<project>/.cursor/`; the user-level plugin
/// owns all three surfaces now. This is the project-level counterpart of the
/// [`LEGACY_PLUGIN_DIRS`] sweep: detection-gated so projects without legacy
/// artifacts are untouched, and only tokensave-owned entries are removed —
/// user-authored config (other MCP servers, custom hooks and rules, and
/// `permissions.json` allowlists, which the plugin README still recommends
/// per-repo) is preserved.
fn sweep_legacy_project_artifacts(project_path: &Path) -> Result<()> {
    let cursor_dir = project_path.join(".cursor");
    let mcp_path = cursor_dir.join("mcp.json");
    let hooks_path = cursor_dir.join("hooks.json");
    let rule_path = cursor_dir.join("rules/tokensave.mdc");
    let legacy_mcp = legacy_mcp_has_tokensave(&mcp_path);
    let legacy_hooks = legacy_hooks_have_tokensave(&hooks_path);
    let legacy_rule = legacy_rule_has_tokensave(&rule_path);
    if !legacy_mcp && !legacy_hooks && !legacy_rule {
        return Ok(());
    }
    for path in [&mcp_path, &hooks_path, &rule_path] {
        super::ensure_project_local_safe_path(project_path, path)?;
    }
    if legacy_mcp {
        uninstall_mcp_server(&mcp_path);
    }
    if legacy_hooks {
        remove_legacy_project_hooks(&hooks_path)?;
    }
    if legacy_rule {
        remove_legacy_project_rule(&rule_path)?;
    }
    Ok(())
}

/// The project directory a cwd-based legacy sweep should target, or `None`
/// when the cwd *is* the home directory — there `.cursor/` is Cursor's
/// user-level config tree, not a project workspace.
fn cwd_sweep_target(cwd: PathBuf, home: &Path) -> Option<PathBuf> {
    let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    (canonical(&cwd) != canonical(home)).then_some(cwd)
}

/// Best-effort [`sweep_legacy_project_artifacts`] for global install /
/// update-plugin / uninstall flows, which have no explicit project path: the
/// current working directory is treated as the project. Failures only warn so
/// a malformed `.cursor/` in an unrelated cwd can never block plugin
/// management.
fn sweep_legacy_project_artifacts_at_cwd(home: &Path) {
    let Some(project_path) = std::env::current_dir()
        .ok()
        .and_then(|cwd| cwd_sweep_target(cwd, home))
    else {
        return;
    };
    if let Err(err) = sweep_legacy_project_artifacts(&project_path) {
        eprintln!(
            "\x1b[33mwarning:\x1b[0m could not remove legacy project Cursor artifacts in {}: {err}",
            project_path.display()
        );
    }
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
        ("sessionEnd", "hook-cursor-session-end"),
        ("subagentStart", "hook-cursor-subagent-start"),
        ("postToolUse", "hook-cursor-post-tool-use"),
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

/// Flags a stalled Cursor transcript ingest. The per-turn hooks cap how much
/// transcript tail they read ([`crate::hooks::CURSOR_CATCH_UP_INGEST_MAX_BYTES`]),
/// so a backlog above that cap will never drain on its own — exactly the
/// "session recall is silently missing recent turns" failure users hit.
fn doctor_check_session_ingest(dc: &mut DoctorCounters, project_path: &Path) {
    let db_path = crate::sessions::cursor::project_session_db_path(project_path);
    if !db_path.exists() {
        return;
    }
    // `healthcheck` is a sync trait method but runs inside the multi-thread
    // tokio runtime, so the bounded DB read runs via block_in_place.
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let health = tokio::task::block_in_place(|| {
        handle.block_on(async {
            let db = crate::sessions::cursor::open_project_session_db(project_path).await?;
            Some(db.session_ingest_health().await)
        })
    });
    let Some(health) = health else {
        dc.warn(&format!(
            "could not open session store {} to check transcript ingest",
            db_path.display()
        ));
        return;
    };
    if health.max_transcript_pending_bytes > crate::hooks::CURSOR_CATCH_UP_INGEST_MAX_BYTES {
        dc.warn(&format!(
            "Cursor transcript ingest looks stalled: a transcript has {} un-ingested \
             byte(s) ({} byte(s) total across {} transcript(s)), exceeding the {} byte \
             per-transcript hook catch-up cap — it will not drain automatically and \
             session recall is missing those turns",
            health.max_transcript_pending_bytes,
            health.pending_bytes,
            health.pending_transcripts,
            crate::hooks::CURSOR_CATCH_UP_INGEST_MAX_BYTES,
        ));
    } else {
        dc.pass(&format!(
            "Cursor transcript ingest healthy ({} transcript(s) tracked, {} pending \
             byte(s), all within the per-transcript hook cap)",
            health.tracked_transcripts, health.pending_bytes
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

        // A representative skill, the agent, and a dispatcher skill also ship,
        // so released installs are no longer missing the bundle that the
        // symlink path provides.
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
            install_dir
                .join("skills/tokensave-map-architecture/SKILL.md")
                .exists(),
            "a representative slash-dispatcher skill should be embedded"
        );
        assert!(
            !install_dir.join("commands").exists(),
            "the deprecated commands surface must not ship"
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

    /// Every `tokensave_*` token mentioned anywhere in the embedded plugin
    /// bundle (skills, rules, agents, commands, README).
    fn embedded_plugin_tool_mentions() -> std::collections::BTreeSet<String> {
        let mut mentions = std::collections::BTreeSet::new();
        for &(_, contents) in EMBEDDED_PLUGIN_FILES {
            let bytes = contents.as_bytes();
            let mut search_from = 0;
            while let Some(found) = contents[search_from..].find("tokensave_") {
                let start = search_from + found;
                let mut end = start + "tokensave_".len();
                while end < bytes.len()
                    && (bytes[end].is_ascii_lowercase()
                        || bytes[end].is_ascii_digit()
                        || bytes[end] == b'_')
                {
                    end += 1;
                }
                let token = contents[start..end].trim_end_matches('_');
                if token.len() > "tokensave_".len() {
                    mentions.insert(token.to_string());
                }
                search_from = end;
            }
        }
        mentions
    }

    /// The full registered tool-name set, independent of host capabilities
    /// (`tokensave_ast_grep_rewrite` is filtered from `get_tool_definitions`
    /// when the external `ast-grep` binary is absent, but it is still a real
    /// tool the bundle legitimately references).
    fn registered_tool_names() -> std::collections::BTreeSet<String> {
        let mut names: std::collections::BTreeSet<String> =
            crate::mcp::tools::get_tool_definitions()
                .into_iter()
                .map(|definition| definition.name)
                .collect();
        names.insert("tokensave_ast_grep_rewrite".to_string());
        names
    }

    /// Guards against the plugin steering agents toward tools that do not
    /// exist: every `tokensave_*` name mentioned in the bundle must be a
    /// registered MCP tool (or an explicitly allow-listed non-tool marker).
    #[test]
    fn plugin_tool_mentions_resolve_to_registered_tools() {
        // `tokensave_metrics` is the savings-report line prefix in tool
        // output, not a tool name.
        const NON_TOOL_MENTIONS: &[&str] = &["tokensave_metrics"];
        let known = registered_tool_names();
        let unknown: Vec<String> = embedded_plugin_tool_mentions()
            .into_iter()
            .filter(|mention| {
                !known.contains(mention) && !NON_TOOL_MENTIONS.contains(&mention.as_str())
            })
            .collect();
        assert!(
            unknown.is_empty(),
            "cursor-plugin mentions tool names missing from get_tool_definitions(): {unknown:?}"
        );
    }

    /// Guards against shipping tools no skill/rule/command ever points an
    /// agent at (the audit found whole tool families with zero usage because
    /// nothing in the bundle referenced them). New tools must either be
    /// referenced somewhere under cursor-plugin/ or consciously allow-listed
    /// here with a reason.
    #[test]
    fn registered_tools_are_referenced_by_the_plugin_bundle() {
        // Currently every registered tool is referenced by the bundle. Add a
        // name here only with a written reason for shipping it unsteered.
        const TOOLS_WITHOUT_PLUGIN_REFERENCE: &[&str] = &[];
        let mentions = embedded_plugin_tool_mentions();
        let missing: Vec<String> = registered_tool_names()
            .into_iter()
            .filter(|name| {
                !mentions.contains(name) && !TOOLS_WITHOUT_PLUGIN_REFERENCE.contains(&name.as_str())
            })
            .collect();
        assert!(
            missing.is_empty(),
            "tools registered in get_tool_definitions() but referenced nowhere under \
             cursor-plugin/ (reference them in a skill or allow-list them): {missing:?}"
        );
    }

    /// The skill index injected into Cursor `sessionStart` context must match
    /// the *model-invocable* skills shipped in the bundle — slash dispatchers
    /// (`disable-model-invocation: true`) are explicit-invoke-only and would
    /// be noise in steering context.
    #[test]
    fn session_context_skill_index_matches_bundle_skills() {
        let mut bundled: Vec<String> = EMBEDDED_PLUGIN_FILES
            .iter()
            .filter_map(|&(relative, contents)| {
                let name = relative
                    .strip_prefix("skills/")
                    .and_then(|rest| rest.strip_suffix("/SKILL.md"))?;
                (!contents.contains("disable-model-invocation: true")).then(|| name.to_string())
            })
            .collect();
        bundled.sort();
        let mut listed: Vec<String> = crate::hooks::CURSOR_PLUGIN_SKILLS
            .iter()
            .map(|skill| (*skill).to_string())
            .collect();
        listed.sort();
        assert_eq!(
            bundled, listed,
            "hooks::CURSOR_PLUGIN_SKILLS must list exactly the model-invocable bundled skills"
        );
    }

    /// The Auto-review allowlist documented in the plugin README must stay in
    /// lockstep with the tools' `readOnlyHint` annotations: every read-only
    /// tool is listed (so it skips the classifier) and no mutating tool is.
    #[test]
    fn readme_mcp_allowlist_matches_read_only_tools() {
        let readme = EMBEDDED_PLUGIN_FILES
            .iter()
            .find(|&&(relative, _)| relative == "README.md")
            .map(|&(_, contents)| contents)
            .expect("plugin README must be embedded");

        let mut listed: Vec<String> = readme
            .lines()
            .filter_map(|line| {
                let entry = line.trim().trim_end_matches(',').trim_matches('"');
                entry
                    .strip_prefix("tokensave:")
                    .filter(|tool| tool.starts_with("tokensave_"))
                    .map(str::to_string)
            })
            .collect();
        listed.sort();
        listed.dedup();

        let mut read_only: Vec<String> = crate::mcp::tools::get_tool_definitions()
            .into_iter()
            .filter(|definition| {
                definition
                    .annotations
                    .as_ref()
                    .and_then(|annotations| annotations.get("readOnlyHint"))
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
            })
            .map(|definition| definition.name)
            .collect();
        read_only.sort();

        assert_eq!(
            listed, read_only,
            "the README mcpAllowlist snippet must list exactly the readOnlyHint=true tools"
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

    /// Upgrading over an older install must sweep bundle directories the
    /// current bundle no longer ships (the deprecated `commands/` surface),
    /// instead of stranding them as unmanaged leftovers forever.
    #[test]
    fn reinstall_sweeps_legacy_commands_dir() {
        let tmp = TempDir::new().unwrap();
        let install_dir = tmp.path().join("tokensave");
        write_embedded_plugin(&install_dir, "tokensave").expect("embedded install should succeed");
        // Simulate a pre-migration install that shipped commands/.
        std::fs::create_dir_all(install_dir.join("commands")).unwrap();
        std::fs::write(
            install_dir.join("commands/tokensave-arch.md"),
            "legacy command",
        )
        .unwrap();

        remove_cursor_plugin_install(&install_dir).expect("replace should succeed");
        assert!(
            !install_dir.exists(),
            "legacy commands/ must be swept so the tokensave-only dir is fully removed"
        );
    }

    /// Upgrading over an install that shipped the pre-rename dispatcher slugs
    /// (`skills/tokensave-arch` → `skills/tokensave-map-architecture`, …) must
    /// sweep the old skill directories instead of leaving Cursor listing both
    /// the old and new command skills.
    #[test]
    fn reinstall_sweeps_pre_rename_dispatcher_skills() {
        let tmp = TempDir::new().unwrap();
        let install_dir = tmp.path().join("tokensave");
        write_embedded_plugin(&install_dir, "tokensave").expect("embedded install should succeed");
        // Simulate a pre-rename install that shipped skills/tokensave-arch/.
        std::fs::create_dir_all(install_dir.join("skills/tokensave-arch")).unwrap();
        std::fs::write(
            install_dir.join("skills/tokensave-arch/SKILL.md"),
            "legacy dispatcher skill",
        )
        .unwrap();

        remove_cursor_plugin_install(&install_dir).expect("replace should succeed");
        assert!(
            !install_dir.exists(),
            "pre-rename dispatcher skill dirs must be swept so the tokensave-only dir is fully removed"
        );
    }

    /// The project-local legacy sweep must remove exactly the tokensave-owned
    /// entries pre-plugin installs wrote (`mcp.json` server entry,
    /// `hook-cursor-*` hooks, the steering rule) while preserving everything
    /// the user authored alongside them.
    #[test]
    fn sweep_removes_legacy_project_artifacts_preserving_user_config() {
        let project = TempDir::new().unwrap();
        let cursor_dir = project.path().join(".cursor");
        std::fs::create_dir_all(cursor_dir.join("rules")).unwrap();
        std::fs::write(
            cursor_dir.join("mcp.json"),
            serde_json::to_string_pretty(&json!({
                "mcpServers": {
                    "tokensave": { "command": "tokensave", "args": ["serve"] },
                    "other": { "url": "https://example.com/mcp" }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            cursor_dir.join("hooks.json"),
            serde_json::to_string_pretty(&json!({
                "hooks": {
                    "sessionStart": [
                        { "command": "tokensave hook-cursor-session-start" },
                        { "command": "./my-hook.sh" }
                    ]
                }
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            cursor_dir.join("rules/tokensave.mdc"),
            "Prefer tokensave MCP tools",
        )
        .unwrap();
        std::fs::write(
            cursor_dir.join("permissions.json"),
            serde_json::to_string_pretty(&json!({
                "mcpAllowlist": ["tokensave:tokensave_search"]
            }))
            .unwrap(),
        )
        .unwrap();

        sweep_legacy_project_artifacts(project.path()).expect("sweep should succeed");

        let mcp = load_json_file(&cursor_dir.join("mcp.json"));
        assert!(
            mcp["mcpServers"].get("tokensave").is_none(),
            "legacy tokensave MCP entry must be removed"
        );
        assert!(
            mcp["mcpServers"].get("other").is_some(),
            "user-authored MCP servers must be preserved"
        );
        let hooks = load_json_file(&cursor_dir.join("hooks.json"));
        let entries = hooks["hooks"]["sessionStart"].as_array().unwrap();
        assert_eq!(
            entries,
            &[json!({ "command": "./my-hook.sh" })],
            "only hook-cursor-* entries may be removed"
        );
        assert!(
            !cursor_dir.join("rules/tokensave.mdc").exists(),
            "the legacy steering rule must be removed"
        );
        let permissions = load_json_file(&cursor_dir.join("permissions.json"));
        assert_eq!(
            permissions["mcpAllowlist"],
            json!(["tokensave:tokensave_search"]),
            "per-repo permissions.json allowlists are README-endorsed user config"
        );
    }

    /// A project whose `.cursor/` only holds user-authored config (no legacy
    /// tokensave artifacts) must come through the sweep byte-identical — no
    /// rewrites, no backups, no deletions.
    #[test]
    fn sweep_is_noop_without_legacy_tokensave_artifacts() {
        let project = TempDir::new().unwrap();
        let cursor_dir = project.path().join(".cursor");
        std::fs::create_dir_all(cursor_dir.join("rules")).unwrap();
        let mcp = serde_json::to_string_pretty(&json!({
            "mcpServers": { "other": { "url": "https://example.com/mcp" } }
        }))
        .unwrap();
        std::fs::write(cursor_dir.join("mcp.json"), &mcp).unwrap();
        // A user file that happens to use the legacy rule filename but not
        // the tokensave-generated contents stays untouched.
        let rule = "---\ndescription: my own rule\n---\nFollow project conventions.\n";
        std::fs::write(cursor_dir.join("rules/tokensave.mdc"), rule).unwrap();

        sweep_legacy_project_artifacts(project.path()).expect("sweep should succeed");

        assert_eq!(
            std::fs::read_to_string(cursor_dir.join("mcp.json")).unwrap(),
            mcp
        );
        assert_eq!(
            std::fs::read_to_string(cursor_dir.join("rules/tokensave.mdc")).unwrap(),
            rule
        );
        let mut files = collect_regular_files(&cursor_dir).unwrap();
        files.sort();
        assert_eq!(
            files,
            vec![
                cursor_dir.join("mcp.json"),
                cursor_dir.join("rules/tokensave.mdc")
            ],
            "a no-op sweep must not create backups or new files"
        );
    }

    /// Projects without a `.cursor/` directory at all are a silent no-op.
    #[test]
    fn sweep_handles_missing_cursor_dir() {
        let project = TempDir::new().unwrap();
        sweep_legacy_project_artifacts(project.path()).expect("sweep should succeed");
        assert!(!project.path().join(".cursor").exists());
    }

    /// The cwd-based sweep must never treat the home directory as a project:
    /// `~/.cursor` is Cursor's user-level config tree.
    #[test]
    fn cwd_sweep_target_skips_home_dir() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        assert_eq!(
            cwd_sweep_target(home.path().to_path_buf(), home.path()),
            None
        );
        assert_eq!(
            cwd_sweep_target(project.path().to_path_buf(), home.path()),
            Some(project.path().to_path_buf())
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
