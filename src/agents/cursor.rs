//! Cursor agent integration.
//!
//! Installs tracedecay's Cursor plugin bundle into Cursor's local plugin
//! directory. The plugin owns MCP, hooks, and rule configuration.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use serde_json::json;

use crate::errors::{Result, TraceDecayError};

use super::{
    AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext, UpdatePluginOutcome,
    backup_and_write_json, load_json_file, load_jsonc_file_strict, safe_write_text_file,
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
        install_cursor_plugin(&ctx.home, &ctx.tracedecay_bin)?;
        sweep_legacy_project_artifacts_at_cwd(&ctx.home);

        eprintln!();
        eprintln!("Setup complete. Next steps:");
        eprintln!("  1. cd into your project and run: tracedecay init");
        eprintln!("  2. Reload Cursor — the tracedecay plugin is now installed");
        eprintln!(
            "  3. Optional: Cursor's Auto-review mode reviews every MCP call; to let \
             tracedecay's read-only tools run without per-call review, copy the \
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
        install_cursor_plugin(&ctx.home, &ctx.tracedecay_bin)?;
        sweep_legacy_project_artifacts(project_path)?;

        eprintln!();
        eprintln!("Cursor local setup uses the tracedecay Cursor plugin.");
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
        // The whole plugin directory is a tracedecay-generated bundle (its
        // mcp.json / hooks.json are rendered artifacts, not user config), so
        // refreshing it is exactly the install path. User config such as
        // `~/.cursor/mcp.json` is never written by `install_cursor_plugin`,
        // and unmanaged files inside the plugin dir are preserved.
        if !cursor_plugin_manifest_path(&ctx.home).exists() {
            return Ok(UpdatePluginOutcome::NotInstalled);
        }
        install_cursor_plugin(&ctx.home, &ctx.tracedecay_bin)?;
        sweep_legacy_project_artifacts_at_cwd(&ctx.home);
        Ok(UpdatePluginOutcome::Refreshed(vec![
            cursor_plugin_install_dir(&ctx.home),
        ]))
    }

    fn export_managed_skills(
        &self,
        home: &Path,
        profile_root: &Path,
    ) -> Result<Vec<crate::automation::skill_targets::SkillInstallSummary>> {
        if !cursor_plugin_manifest_path(home).exists() {
            return Ok(Vec::new());
        }
        Ok(vec![
            crate::automation::skill_targets::install_managed_skills(
                profile_root,
                crate::automation::skill_targets::SkillInstallTarget::Cursor,
                &cursor_plugin_install_dir(home),
            )?,
        ])
    }

    fn uninstall(&self, ctx: &InstallContext) -> Result<()> {
        let install_dir = cursor_plugin_install_dir(&ctx.home);
        let profile_root = crate::automation::skill_targets::profile_root_for_agent_home(&ctx.home);
        crate::automation::memory_digest::remove_memory_digest_export(
            &profile_root,
            crate::automation::skill_targets::SkillInstallTarget::Cursor,
            &install_dir,
        )?;
        remove_cursor_plugin_install(&install_dir)?;
        let mcp_path = ctx.home.join(".cursor/mcp.json");
        uninstall_mcp_server(&mcp_path);
        sweep_legacy_project_artifacts_at_cwd(&ctx.home);

        eprintln!();
        eprintln!("Uninstall complete. TraceDecay has been removed from Cursor.");
        eprintln!("Restart Cursor for changes to take effect.");
        Ok(())
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mCursor integration\x1b[0m");
        let project_cursor = ctx.project_path.join(".cursor");
        doctor_check_plugin(dc, &ctx.home);
        if legacy_project_cursor_has_tracedecay(&project_cursor) {
            dc.warn(
                "legacy project Cursor MCP/hooks/rule files are present; rerun \
                 `tracedecay install --agent cursor` from this project to remove \
                 tracedecay-owned entries",
            );
        }
        doctor_check_session_ingest(dc, &ctx.project_path);
        super::cursor_diagnostics::report_cursor_mcp_log_findings(dc, &ctx.home);
    }

    fn is_detected(&self, home: &Path) -> bool {
        home.join(".cursor").is_dir()
    }

    fn primary_config_path(&self, home: &Path) -> Option<std::path::PathBuf> {
        Some(cursor_plugin_manifest_path(home))
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        cursor_plugin_manifest_path(home).exists()
            || legacy_mcp_has_tracedecay(&home.join(".cursor/mcp.json"))
    }
}

// ---------------------------------------------------------------------------
// Post-install hook
// ---------------------------------------------------------------------------

/// Registers the project's current git branch for tracedecay indexing after a
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
    // Materialize the always-applied memory rule from this project's fact
    // store so install/update-plugin leaves fresh memory in place instead of
    // waiting for the first sessionStart hook. Fail-open, no-op when the
    // project has no initialized store.
    crate::hooks::memory_inject::regenerate_cursor_memory_rule(project_path).await;
    let Some(branch_name) = crate::branch::current_branch(project_path) else {
        return;
    };
    match crate::tracedecay::TraceDecay::add_branch_tracking(project_path, &branch_name).await {
        Ok(crate::branch::BranchAddOutcome::Added) => {
            eprintln!(
                "\x1b[32m✔\x1b[0m Tracked Cursor branch '{branch_name}' for tracedecay indexing"
            );
        }
        Ok(
            crate::branch::BranchAddOutcome::AlreadyTracked
            | crate::branch::BranchAddOutcome::Deferred
            | crate::branch::BranchAddOutcome::NotIndexed,
        ) => {}
        Err(err) => {
            eprintln!(
                "\x1b[33mwarning:\x1b[0m could not track Cursor branch '{branch_name}' for tracedecay indexing: {err}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Plugin install helpers
// ---------------------------------------------------------------------------

/// The Cursor plugin's composed deploy set, sourced from the shared
/// `plugin/` tree via [`crate::agents::plugin_bundle::cursor_files`].
/// Each entry is `(deploy_relative_path, file_contents)`. The manifest,
/// `mcp.json`, and `hooks/hooks.json` entries are rendered through helpers at
/// install time to inject the package version and the absolute tracedecay
/// binary path.
fn embedded_plugin_files() -> Vec<(&'static str, &'static str)> {
    crate::agents::plugin_bundle::cursor_files()
}

fn cursor_plugin_install_dir(home: &Path) -> PathBuf {
    home.join(".cursor/plugins/local/tracedecay")
}

fn cursor_plugin_manifest_path(home: &Path) -> PathBuf {
    cursor_plugin_install_dir(home).join(".cursor-plugin/plugin.json")
}

/// Path of the materialized always-applied memory rule rendered from the
/// project fact store (see `hooks::memory_inject::regenerate_cursor_memory_rule`).
/// The install path writes the embedded placeholder; hooks rewrite it in
/// place with rendered facts.
pub fn cursor_memory_rule_path(home: &Path) -> PathBuf {
    cursor_plugin_install_dir(home).join("rules/tracedecay-memory.mdc")
}

fn install_cursor_plugin(home: &Path, tracedecay_bin: &str) -> Result<()> {
    let install_dir = cursor_plugin_install_dir(home);
    if let Some(parent) = install_dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| TraceDecayError::Config {
            message: format!("failed to create {}: {e}", parent.display()),
        })?;
    }
    remove_cursor_plugin_install(&install_dir)?;

    write_embedded_plugin(&install_dir, tracedecay_bin)?;
    install_cursor_managed_skill_overlay(home, &install_dir)?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Installed Cursor plugin at {}",
        install_dir.display()
    );
    Ok(())
}

fn install_cursor_managed_skill_overlay(home: &Path, install_dir: &Path) -> Result<()> {
    let profile_root = crate::automation::skill_targets::profile_root_for_agent_home(home);
    crate::automation::skill_targets::install_managed_skills(
        &profile_root,
        crate::automation::skill_targets::SkillInstallTarget::Cursor,
        install_dir,
    )?;
    crate::automation::memory_digest::sync_memory_digest_export(
        &profile_root,
        crate::automation::skill_targets::SkillInstallTarget::Cursor,
        install_dir,
    )?;
    Ok(())
}

fn write_embedded_plugin(install_dir: &Path, tracedecay_bin: &str) -> Result<()> {
    for (relative, contents) in embedded_plugin_files() {
        let rendered = match relative {
            ".cursor-plugin/plugin.json" => cursor_plugin_manifest(contents)?,
            "mcp.json" => cursor_plugin_mcp(contents, tracedecay_bin)?,
            "hooks/hooks.json" => cursor_plugin_hooks(contents, tracedecay_bin)?,
            _ => contents.to_string(),
        };
        safe_write_text_file(&install_dir.join(relative), &rendered, None)?;
    }
    Ok(())
}

fn cursor_plugin_manifest(raw: &str) -> Result<String> {
    super::plugin_bundle::stamp_manifest_version(raw)
}

fn cursor_plugin_mcp(raw: &str, tracedecay_bin: &str) -> Result<String> {
    super::plugin_bundle::set_mcp_command(raw, tracedecay_bin)
}

fn cursor_plugin_hooks(raw: &str, tracedecay_bin: &str) -> Result<String> {
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
                    if let Some(suffix) = command.strip_prefix("tracedecay ") {
                        *command_value = json!(super::hook_command(tracedecay_bin, suffix));
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
        std::fs::remove_file(install_dir).map_err(|e| TraceDecayError::Config {
            message: format!("failed to remove {}: {e}", install_dir.display()),
        })?;
        return Ok(());
    }
    if !metadata.is_dir() {
        return Err(TraceDecayError::Config {
            message: format!(
                "refusing to replace non-directory Cursor plugin path {}",
                install_dir.display()
            ),
        });
    }
    if !cursor_plugin_dir_is_tracedecay(install_dir) {
        return Err(TraceDecayError::Config {
            message: format!(
                "refusing to replace unmanaged Cursor plugin directory {}",
                install_dir.display()
            ),
        });
    }
    // The directory is tracedecay-owned. Sweep every skill dir the *current*
    // bundle no longer ships (retired dispatcher/workflow/memory skills), then
    // remove the managed skill overlay. Deriving the keep-set from the live
    // bundle means a newly retired skill is swept automatically — no
    // hand-maintained legacy list to fall out of date. User-added files
    // outside `skills/` (and any non-tracedecay skill dir) are preserved.
    sweep_retired_bundle_skill_dirs(install_dir);
    remove_cursor_managed_skill_overlay(install_dir);
    if cursor_plugin_dir_has_only_managed_files(install_dir) {
        std::fs::remove_dir_all(install_dir).map_err(|e| TraceDecayError::Config {
            message: format!("failed to remove {}: {e}", install_dir.display()),
        })?;
    } else {
        for path in cursor_plugin_managed_paths(install_dir) {
            std::fs::remove_file(&path).ok();
        }
    }
    Ok(())
}

fn remove_cursor_managed_skill_overlay(install_dir: &Path) {
    std::fs::remove_dir_all(install_dir.join("skills/agent-managed")).ok();
}

/// Remove every `skills/<dir>` under the tracedecay plugin dir that the current
/// bundle does not ship. The keep-set is derived from the live embedded bundle,
/// so any retired skill (dispatcher, workflow, or merged-away memory skill) is
/// swept on upgrade without a hand-maintained legacy list. The `agent-managed`
/// overlay is preserved here (removed separately) and never counted as retired.
///
/// Only tracedecay-owned skill dirs are swept: a same-name user-authored skill
/// whose `SKILL.md` carries no tracedecay marker is left untouched, so an
/// upgrade never deletes a user's private workflow that happens to collide with
/// a retired bundle slug.
fn sweep_retired_bundle_skill_dirs(install_dir: &Path) {
    let skills_root = install_dir.join("skills");
    let Ok(entries) = std::fs::read_dir(&skills_root) else {
        return;
    };
    let shipped: std::collections::BTreeSet<String> = embedded_plugin_files()
        .into_iter()
        .filter_map(|(relative, _)| {
            relative
                .strip_prefix("skills/")
                .and_then(|rest| rest.split('/').next())
                .map(str::to_string)
        })
        .collect();
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        // The managed overlay is handled separately; never treat it as retired.
        if name == "agent-managed" || shipped.contains(&name) {
            continue;
        }
        // Preserve user-authored skills that reuse a retired slug: only sweep a
        // non-shipped dir that is demonstrably tracedecay-owned.
        if !skill_file_has_tracedecay_marker(&entry.path().join("SKILL.md")) {
            continue;
        }
        std::fs::remove_dir_all(entry.path()).ok();
    }
}

/// True when a Cursor `SKILL.md` carries a tracedecay authorship marker, marking
/// the skill dir as tracedecay-owned (and therefore safe to sweep when retired).
fn skill_file_has_tracedecay_marker(skill_file: &Path) -> bool {
    std::fs::read_to_string(skill_file)
        .is_ok_and(|contents| super::skill_contents_have_tracedecay_marker(&contents))
}

fn cursor_plugin_dir_is_tracedecay(install_dir: &Path) -> bool {
    let manifest = load_json_file(&install_dir.join(".cursor-plugin/plugin.json"));
    matches!(
        manifest.get("name").and_then(|v| v.as_str()),
        Some("tracedecay")
    )
}

fn cursor_plugin_dir_has_only_managed_files(install_dir: &Path) -> bool {
    let Ok(entries) = super::collect_regular_files(install_dir) else {
        return false;
    };
    let managed = cursor_plugin_managed_paths(install_dir);
    entries.iter().all(|entry| managed.contains(entry))
}

fn cursor_plugin_managed_paths(install_dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = embedded_plugin_files()
        .into_iter()
        .map(|(relative, _)| install_dir.join(relative))
        .collect();
    paths.push(install_dir.join("rules/tracedecay-memory-digest.mdc"));
    paths
}

fn legacy_mcp_has_tracedecay(mcp_path: &Path) -> bool {
    load_json_file(mcp_path)
        .get("mcpServers")
        .is_some_and(|servers| servers.get("tracedecay").is_some())
}

fn legacy_project_cursor_has_tracedecay(cursor_dir: &Path) -> bool {
    legacy_mcp_has_tracedecay(&cursor_dir.join("mcp.json"))
        || legacy_hooks_have_tracedecay(&cursor_dir.join("hooks.json"))
        || legacy_rule_has_tracedecay(&cursor_dir.join("rules/tracedecay.mdc"))
}

/// Removes legacy PROJECT-local tracedecay artifacts. Pre-plugin versions of
/// `tracedecay install --local` wrote the MCP server entry, lifecycle hooks,
/// and the steering rule into `<project>/.cursor/`; the user-level plugin
/// owns all three surfaces now. This is the project-level counterpart of the
/// user-level plugin-dir clean replace: detection-gated so projects without
/// legacy artifacts are untouched, and only tracedecay-owned entries are removed —
/// user-authored config (other MCP servers, custom hooks and rules, and
/// `permissions.json` allowlists, which the plugin README still recommends
/// per-repo) is preserved.
fn sweep_legacy_project_artifacts(project_path: &Path) -> Result<()> {
    let cursor_dir = project_path.join(".cursor");
    let mcp_path = cursor_dir.join("mcp.json");
    let hooks_path = cursor_dir.join("hooks.json");
    let rule_paths = [cursor_dir.join("rules/tracedecay.mdc")];
    let legacy_mcp = legacy_mcp_has_tracedecay(&mcp_path);
    let legacy_hooks = legacy_hooks_have_tracedecay(&hooks_path);
    let legacy_rule = rule_paths
        .iter()
        .any(|path| legacy_rule_has_tracedecay(path));
    if !legacy_mcp && !legacy_hooks && !legacy_rule {
        return Ok(());
    }
    for path in [&mcp_path, &hooks_path] {
        super::ensure_project_local_safe_path(project_path, path)?;
    }
    for path in &rule_paths {
        super::ensure_project_local_safe_path(project_path, path)?;
    }
    if legacy_mcp {
        uninstall_mcp_server(&mcp_path);
    }
    if legacy_hooks {
        remove_legacy_project_hooks(&hooks_path)?;
    }
    if legacy_rule {
        for path in &rule_paths {
            remove_legacy_project_rule(path)?;
        }
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

/// A Cursor hook entry is tracedecay-owned when its `command` runs a
/// `hook-cursor-*` subcommand.
fn is_legacy_tracedecay_hook(entry: &serde_json::Value) -> bool {
    entry
        .get("command")
        .and_then(|value| value.as_str())
        .is_some_and(|command| command.contains("hook-cursor-"))
}

fn legacy_hooks_have_tracedecay(hooks_path: &Path) -> bool {
    load_json_file(hooks_path)
        .get("hooks")
        .and_then(|value| value.as_object())
        .is_some_and(|events| {
            events.values().any(|value| {
                value
                    .as_array()
                    .is_some_and(|entries| entries.iter().any(is_legacy_tracedecay_hook))
            })
        })
}

fn legacy_rule_has_tracedecay(rule_path: &Path) -> bool {
    std::fs::read_to_string(rule_path)
        .is_ok_and(|contents| contents.contains("tracedecay MCP tools"))
}

/// Remove the tracedecay MCP server entry from a Cursor `mcp.json`, deleting the
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
            "  No tracedecay MCP server in {}, skipping",
            mcp_path.display()
        );
        return;
    };

    let removed = servers.remove("tracedecay").is_some();
    if !removed {
        eprintln!(
            "  No tracedecay MCP server in {}, skipping",
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
            "\x1b[32m✔\x1b[0m Removed tracedecay MCP server from {}",
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
        entries.retain(|entry| !is_legacy_tracedecay_hook(entry));
        removed |= entries.len() != before;
    }
    events.retain(|_, value| value.as_array().is_none_or(|entries| !entries.is_empty()));

    if !removed {
        return Ok(());
    }
    if events.is_empty() {
        std::fs::remove_file(hooks_path).map_err(|e| TraceDecayError::Config {
            message: format!("failed to remove {}: {e}", hooks_path.display()),
        })?;
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed legacy Cursor hooks from {}",
            hooks_path.display()
        );
    } else if backup_and_write_json(hooks_path, &hooks) {
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed legacy Cursor hooks from {}",
            hooks_path.display()
        );
    }
    Ok(())
}

fn remove_legacy_project_rule(rule_path: &Path) -> Result<()> {
    if !rule_path.exists() {
        return Ok(());
    }
    let contents = std::fs::read_to_string(rule_path).map_err(|e| TraceDecayError::Config {
        message: format!("failed to read {}: {e}", rule_path.display()),
    })?;
    if contents.contains("tracedecay MCP tools") {
        std::fs::remove_file(rule_path).map_err(|e| TraceDecayError::Config {
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
            "{} not found — run `tracedecay install --agent cursor` if you use Cursor",
            manifest_path.display()
        ));
        if legacy_mcp_has_tracedecay(&home.join(".cursor/mcp.json")) {
            dc.warn(
                "legacy Cursor MCP config is installed; rerun install to use the Cursor plugin",
            );
        }
        return;
    }

    let manifest = load_json_file(&manifest_path);
    if manifest.get("name").and_then(|v| v.as_str()) == Some("tracedecay")
        && manifest.get("mcpServers").and_then(|v| v.as_str()) == Some("mcp.json")
        && manifest.get("hooks").and_then(|v| v.as_str()) == Some("hooks/hooks.json")
    {
        dc.pass(&format!(
            "Cursor plugin manifest active in {}",
            manifest_path.display()
        ));
    } else {
        dc.fail(&format!(
            "Cursor tracedecay plugin manifest is incomplete in {}",
            manifest_path.display()
        ));
    }
    if let Some(message) =
        super::cursor_diagnostics::plugin_version_staleness(&manifest, env!("CARGO_PKG_VERSION"))
    {
        dc.warn(&message);
    }
    doctor_check_plugin_mcp(dc, &plugin_dir.join("mcp.json"));
    doctor_check_plugin_hooks(dc, &plugin_dir.join("hooks/hooks.json"));
    doctor_check_plugin_rule(dc, &plugin_dir.join("rules/tracedecay.mdc"));
}

fn doctor_check_plugin_mcp(dc: &mut DoctorCounters, mcp_path: &Path) {
    if !mcp_path.exists() {
        dc.warn(&format!(
            "{} not found — run `tracedecay install --agent cursor`",
            mcp_path.display()
        ));
        return;
    }
    let settings = load_json_file(mcp_path);
    // Cursor Settings surfaces the MCP server key literally, so the Cursor
    // plugin registers `tracedecay` (not the Claude/Codex `graph` key).
    let server = &settings["mcpServers"]["tracedecay"];
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
            "Cursor plugin MCP config is incomplete in {} — run `tracedecay install --agent cursor`",
            mcp_path.display()
        ));
    }
}

/// `(event, hook subcommand)` pairs parsed from the embedded plugin
/// `hooks/hooks.json` template, so the doctor check can never drift from the
/// hooks the bundle actually registers.
fn cursor_plugin_hook_expectations() -> Vec<(String, String)> {
    let files = embedded_plugin_files();
    let raw = files
        .iter()
        .find(|(relative, _)| *relative == "hooks/hooks.json")
        .map_or("{}", |&(_, contents)| contents);
    let template: serde_json::Value = serde_json::from_str(raw).unwrap_or_else(|_| json!({}));
    let Some(events) = template.get("hooks").and_then(|hooks| hooks.as_object()) else {
        return Vec::new();
    };
    events
        .iter()
        .flat_map(|(event, entries)| {
            entries
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|entry| {
                    entry["command"]
                        .as_str()
                        .and_then(|command| command.strip_prefix("tracedecay "))
                        .map(|subcommand| (event.clone(), subcommand.to_string()))
                })
        })
        .collect()
}

fn doctor_check_plugin_hooks(dc: &mut DoctorCounters, hooks_path: &Path) {
    if !hooks_path.exists() {
        dc.warn(&format!(
            "{} not found — run `tracedecay install --agent cursor`",
            hooks_path.display()
        ));
        return;
    }
    let hooks = load_jsonc_file_strict(hooks_path).unwrap_or_else(|e| {
        dc.fail(&format!("{e}"));
        json!({})
    });
    let expected = cursor_plugin_hook_expectations();
    let missing: Vec<&str> = expected
        .iter()
        .filter_map(|(event, command)| {
            let has = hooks["hooks"][event.as_str()]
                .as_array()
                .is_some_and(|entries| {
                    entries.iter().any(|entry| {
                        entry["command"]
                            .as_str()
                            .is_some_and(|value| value.contains(command))
                    })
                });
            (!has).then_some(event.as_str())
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
            "Cursor plugin hook(s) missing for {} — run `tracedecay install --agent cursor`",
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
            let placeholder_paths = db.literal_workspace_placeholder_transcript_paths(10).await;
            if !placeholder_paths.is_empty() {
                dc.warn(&format!(
                    "Cursor transcript ingest has {} path(s) with a literal workspace placeholder; \
                     Cursor did not expand `${{workspaceFolder}}`, so session recall will miss those transcripts",
                    placeholder_paths.len(),
                ));
                for path in &placeholder_paths {
                    dc.info(&format!("  - {path}"));
                }
            }
            Some(db.session_ingest_health_for_provider(Some("cursor")).await)
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
             session recall is missing those turns. Run `tracedecay sessions ingest \
             --project-path {}` to drain the backlog manually",
            health.max_transcript_pending_bytes,
            health.pending_bytes,
            health.pending_transcripts,
            crate::hooks::CURSOR_CATCH_UP_INGEST_MAX_BYTES,
            project_path.display(),
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
            "{} not found — run `tracedecay install --agent cursor`",
            rule_path.display()
        ));
        return;
    }
    let contents = std::fs::read_to_string(rule_path).unwrap_or_default();
    if contents.contains("alwaysApply: true") && contents.contains("tracedecay MCP tools") {
        dc.pass(&format!(
            "Cursor plugin tracedecay rule active in {}",
            rule_path.display()
        ));
    } else {
        dc.fail(&format!(
            "Cursor plugin tracedecay rule is incomplete in {} — run `tracedecay install --agent cursor`",
            rule_path.display()
        ));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn plugin_source_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin")
    }

    /// Directory names directly under `plugin/skills/` on disk.
    fn shared_skill_dirs() -> Vec<String> {
        subdir_names(&plugin_source_root().join("skills"))
    }

    /// File names under `plugin/overlays/cursor/commands/` (the Cursor native
    /// slash-command markdown files).
    fn cursor_command_files() -> Vec<String> {
        let root = plugin_source_root().join("overlays/cursor/commands");
        let mut names: Vec<String> = std::fs::read_dir(&root)
            .expect("plugin cursor commands dir should be readable")
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|t| t.is_file()))
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn subdir_names(root: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(root)
            .expect("plugin source dir should be readable")
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// Every file under a single skill dir, relative to it, forward-slashed.
    fn skill_dir_tree_files(skill_dir: &Path) -> Vec<String> {
        let mut files: Vec<String> = crate::agents::collect_regular_files(skill_dir)
            .expect("skill dir readable")
            .into_iter()
            .filter_map(|path| {
                path.strip_prefix(skill_dir)
                    .ok()
                    .map(|rel| rel.to_string_lossy().replace('\\', "/"))
            })
            .collect();
        files.sort();
        files
    }

    /// The doctor's expected-hooks list is parsed from the embedded bundle
    /// template; a parse regression would silently disable the hook checks.
    #[test]
    fn plugin_hook_expectations_cover_the_bundled_hooks() {
        let expectations = cursor_plugin_hook_expectations();
        assert_eq!(
            expectations.len(),
            9,
            "expected one entry per bundled lifecycle hook, got {expectations:?}"
        );
        assert!(expectations.contains(&(
            "sessionStart".to_string(),
            "hook-cursor-session-start".to_string()
        )));
        assert!(expectations.contains(&(
            "afterFileEdit".to_string(),
            "hook-cursor-after-file-edit".to_string()
        )));
    }

    #[test]
    fn write_embedded_plugin_writes_core_and_bundle_files() {
        let tmp = TempDir::new().unwrap();
        let install_dir = tmp.path().join("tracedecay");
        write_embedded_plugin(&install_dir, "tracedecay").expect("embedded install should succeed");

        // The four core files land, and the manifest is valid JSON carrying the
        // mcpServers key released binaries rely on.
        let manifest_path = install_dir.join(".cursor-plugin/plugin.json");
        let manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
        assert_eq!(manifest["name"], "tracedecay");
        assert_eq!(manifest["mcpServers"], "mcp.json");
        assert!(install_dir.join("README.md").exists());
        assert!(install_dir.join("mcp.json").exists());
        assert!(install_dir.join("hooks/hooks.json").exists());
        assert!(install_dir.join("rules/tracedecay.mdc").exists());

        // Cursor Settings surfaces the MCP server key literally, so the
        // Cursor plugin must register `tracedecay` (not Claude/Codex `graph`).
        let mcp: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(install_dir.join("mcp.json")).unwrap())
                .unwrap();
        let server = &mcp["mcpServers"]["tracedecay"];
        assert!(
            server.is_object(),
            "Cursor mcp.json must declare mcpServers.tracedecay"
        );
        assert_eq!(server["command"], "tracedecay");
        assert_eq!(
            server["args"],
            serde_json::json!(["serve", "--path", "${workspaceFolder}"])
        );
        assert!(
            mcp["mcpServers"].get("graph").is_none(),
            "Cursor mcp.json must not keep the Claude/Codex graph key"
        );

        // A representative skill, the agent, and a native slash command also
        // ship, so released installs are no longer missing the bundle that the
        // symlink path provides.
        assert!(
            install_dir.join("skills/exploring-code/SKILL.md").exists(),
            "a representative skill should be embedded"
        );
        assert!(
            install_dir.join("agents/code-explorer.md").exists(),
            "the code-explorer agent should be embedded"
        );
        assert!(
            install_dir
                .join("commands/tracedecay-map-architecture.md")
                .exists(),
            "a representative native slash command should be embedded"
        );
        // Cursor no longer ships the `tracedecay-*` dispatcher *skills* — those
        // slugs are native commands now.
        assert!(
            !install_dir
                .join("skills/tracedecay-map-architecture/SKILL.md")
                .exists(),
            "the retired dispatcher skill must not ship"
        );

        // Every embedded file is also a managed path so uninstall can clean it.
        let managed = cursor_plugin_managed_paths(&install_dir);
        for (relative, _) in embedded_plugin_files() {
            assert!(
                managed.contains(&install_dir.join(relative)),
                "{relative} should be a managed path"
            );
        }
    }

    /// The Cursor deploy set (composed from the shared `plugin/` tree) must
    /// cover every shared *model-invocable* skill (all non-`tracedecay-*`
    /// slugs), every native slash command, and Cursor's manifest/rules/agents —
    /// with no on-disk skill left unwired. The source paths under `plugin/`
    /// differ from the deploy paths, so this checks the *composition*, not a raw
    /// dir walk.
    #[test]
    fn embedded_file_list_covers_the_whole_source_bundle() {
        let deploy: std::collections::BTreeSet<String> = embedded_plugin_files()
            .into_iter()
            .map(|(relative, _)| relative.to_string())
            .collect();

        // Every file under each shared model-invocable skill dir (SKILL.md and
        // any support files) must be deployed by Cursor. The `tracedecay-*`
        // dispatcher skills are NOT shipped to Cursor — they are native commands
        // there.
        let skills_root = plugin_source_root().join("skills");
        for skill in shared_skill_dirs() {
            let skill_dir = skills_root.join(&skill);
            if skill.starts_with("tracedecay-") {
                let dispatcher = format!("skills/{skill}/SKILL.md");
                assert!(
                    !deploy.contains(&dispatcher),
                    "Cursor deploy set must NOT ship dispatcher skill {dispatcher}"
                );
                continue;
            }
            for relative in skill_dir_tree_files(&skill_dir) {
                let expected = format!("skills/{skill}/{relative}");
                assert!(
                    deploy.contains(&expected),
                    "Cursor deploy set is missing skill file {expected}"
                );
            }
        }
        // Every Cursor native slash command must be deployed.
        for command in cursor_command_files() {
            let expected = format!("commands/{command}");
            assert!(
                deploy.contains(&expected),
                "Cursor deploy set is missing native command {expected}"
            );
        }
        // Cursor's manifest surfaces.
        for expected in [
            ".cursor-plugin/plugin.json",
            "mcp.json",
            "hooks/hooks.json",
            "README.md",
            "rules/tracedecay.mdc",
            "rules/tracedecay-memory.mdc",
        ] {
            assert!(
                deploy.contains(expected),
                "Cursor deploy set is missing {expected}"
            );
        }

        // Every canonical agent is rendered into Cursor's generated deploy
        // set. Directory discovery prevents a newly added catalog entry from
        // being omitted by adapter generation.
        let agents_root = plugin_source_root().join("agents");
        for entry in std::fs::read_dir(&agents_root).expect("canonical agent catalog readable") {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            let expected = format!("agents/{name}");
            assert!(
                deploy.contains(&expected),
                "Cursor deploy set is missing agent {expected}"
            );
        }
    }

    /// Every `tracedecay_*` token mentioned anywhere in the embedded plugin
    /// bundle (skills, rules, agents, commands, README).
    fn embedded_plugin_tool_mentions() -> std::collections::BTreeSet<String> {
        let mut mentions = std::collections::BTreeSet::new();
        for (_, contents) in embedded_plugin_files() {
            let bytes = contents.as_bytes();
            let mut search_from = 0;
            while let Some(found) = contents[search_from..].find("tracedecay_") {
                let start = search_from + found;
                let mut end = start + "tracedecay_".len();
                while end < bytes.len()
                    && (bytes[end].is_ascii_lowercase()
                        || bytes[end].is_ascii_digit()
                        || bytes[end] == b'_')
                {
                    end += 1;
                }
                let token = contents[start..end].trim_end_matches('_');
                if token.len() > "tracedecay_".len() {
                    mentions.insert(token.to_string());
                }
                search_from = end;
            }
        }
        mentions
    }

    /// The full registered tool-name set, independent of host capabilities
    /// (`tracedecay_ast_grep_rewrite` is filtered from `get_tool_definitions`
    /// when the external `ast-grep` binary is absent, but it is still a real
    /// tool the bundle legitimately references).
    fn registered_tool_names() -> std::collections::BTreeSet<String> {
        let mut names: std::collections::BTreeSet<String> =
            crate::mcp::tools::get_tool_definitions()
                .into_iter()
                .map(|definition| definition.name)
                .collect();
        names.insert("tracedecay_ast_grep_rewrite".to_string());
        names
    }

    /// Guards against the plugin steering agents toward tools that do not
    /// exist: every `tracedecay_*` name mentioned in the bundle must be a
    /// registered MCP tool (or an explicitly allow-listed non-tool marker).
    #[test]
    fn plugin_tool_mentions_resolve_to_registered_tools() {
        // `tracedecay_metrics` is the savings-report line prefix in tool
        // output, not a tool name.
        const NON_TOOL_MENTIONS: &[&str] = &["tracedecay_metrics"];
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
        let mut bundled: Vec<String> = embedded_plugin_files()
            .into_iter()
            .filter_map(|(relative, contents)| {
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
        let files = embedded_plugin_files();
        let readme = files
            .iter()
            .find(|&&(relative, _)| relative == "README.md")
            .map(|&(_, contents)| contents)
            .expect("plugin README must be embedded");

        let mut listed: Vec<String> = readme
            .lines()
            .filter_map(|line| {
                let entry = line.trim().trim_end_matches(',').trim_matches('"');
                entry
                    .strip_prefix("tracedecay:")
                    .filter(|tool| tool.starts_with("tracedecay_"))
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
        let install_dir = tmp.path().join("tracedecay");
        write_embedded_plugin(&install_dir, "tracedecay").expect("embedded install should succeed");
        assert!(install_dir.join("skills/exploring-code/SKILL.md").exists());

        // Because managed paths cover every embedded file, uninstall recognises a
        // tracedecay-only directory and removes it entirely.
        remove_cursor_plugin_install(&install_dir).expect("uninstall should succeed");
        assert!(
            !install_dir.exists(),
            "embedded install should be fully removed on uninstall"
        );
    }

    /// Upgrading over an install that shipped the `tracedecay-*` dispatcher
    /// *skills* (now re-expressed as native `commands/` slash commands) must
    /// sweep those retired skill dirs instead of stranding them as unmanaged
    /// leftovers, so Cursor does not list both the retired dispatcher skill and
    /// the new native command.
    #[test]
    fn reinstall_sweeps_retired_dispatcher_skill_dirs() {
        let tmp = TempDir::new().unwrap();
        let install_dir = tmp.path().join("tracedecay");
        write_embedded_plugin(&install_dir, "tracedecay").expect("embedded install should succeed");
        // Simulate a pre-migration install that shipped the dispatcher skill.
        std::fs::create_dir_all(install_dir.join("skills/tracedecay-review-diff")).unwrap();
        std::fs::write(
            install_dir.join("skills/tracedecay-review-diff/SKILL.md"),
            "---\nname: tracedecay-review-diff\n---\nApply the `tracedecay:reviewing-changes` skill.\n",
        )
        .unwrap();
        // Also simulate a released install that still ships one of the retired
        // memory skills merged into `project-memory`; the clean replace must
        // sweep it too.
        std::fs::create_dir_all(install_dir.join("skills/recalling-project-memory")).unwrap();
        std::fs::write(
            install_dir.join("skills/recalling-project-memory/SKILL.md"),
            "---\nname: recalling-project-memory\n---\nRecall facts with `tracedecay_fact_store`.\n",
        )
        .unwrap();

        remove_cursor_plugin_install(&install_dir).expect("replace should succeed");
        assert!(
            !install_dir.exists(),
            "retired dispatcher skill dirs must be swept so the tracedecay-only dir is fully removed"
        );
    }

    /// Upgrading over an install that shipped the pre-rename dispatcher slugs
    /// (`skills/tracedecay-arch` → `skills/tracedecay-map-architecture`, …) must
    /// sweep the old skill directories instead of leaving Cursor listing both
    /// the old and new command skills.
    #[test]
    fn reinstall_sweeps_pre_rename_dispatcher_skills() {
        let tmp = TempDir::new().unwrap();
        let install_dir = tmp.path().join("tracedecay");
        write_embedded_plugin(&install_dir, "tracedecay").expect("embedded install should succeed");
        // Simulate a pre-rename install that shipped skills/tracedecay-arch/.
        std::fs::create_dir_all(install_dir.join("skills/tracedecay-arch")).unwrap();
        std::fs::write(
            install_dir.join("skills/tracedecay-arch/SKILL.md"),
            "---\nname: tracedecay-arch\n---\nApply the `tracedecay:code-health` skill.\n",
        )
        .unwrap();

        remove_cursor_plugin_install(&install_dir).expect("replace should succeed");
        assert!(
            !install_dir.exists(),
            "pre-rename dispatcher skill dirs must be swept so the tracedecay-only dir is fully removed"
        );
    }

    /// A reinstall must be a CLEAN REPLACE of the tracedecay-owned dir: a stale
    /// file the current bundle no longer ships is gone afterward, while the
    /// fresh bundle is present. Exercises the full write → remove → write path.
    #[test]
    fn reinstall_is_a_clean_replace_dropping_stale_files() {
        let tmp = TempDir::new().unwrap();
        let install_dir = tmp.path().join("tracedecay");
        write_embedded_plugin(&install_dir, "tracedecay").expect("first install should succeed");
        // A stale skill dir the current bundle does not ship.
        std::fs::create_dir_all(install_dir.join("skills/totally-retired-skill")).unwrap();
        std::fs::write(
            install_dir.join("skills/totally-retired-skill/SKILL.md"),
            "---\nname: totally-retired-skill\n---\nRun `tracedecay_search` first.\n",
        )
        .unwrap();

        // A clean replace: remove the owned dir, then write the fresh bundle.
        remove_cursor_plugin_install(&install_dir).expect("clean replace should succeed");
        write_embedded_plugin(&install_dir, "tracedecay").expect("re-install should succeed");

        assert!(
            !install_dir.join("skills/totally-retired-skill").exists(),
            "a stale skill dir must be gone after a clean-replace reinstall"
        );
        assert!(
            install_dir.join("skills/exploring-code/SKILL.md").exists(),
            "the current bundle must be present after reinstall"
        );
    }

    /// The clean replace must refuse to delete a directory tracedecay does not
    /// own (no tracedecay plugin manifest), so it never nukes an unrelated dir.
    #[test]
    fn clean_replace_refuses_unmanaged_dir() {
        let tmp = TempDir::new().unwrap();
        let install_dir = tmp.path().join("tracedecay");
        std::fs::create_dir_all(&install_dir).unwrap();
        std::fs::write(install_dir.join("user-file.txt"), "not tracedecay").unwrap();

        let err = remove_cursor_plugin_install(&install_dir)
            .expect_err("must refuse an unmanaged directory");
        assert!(
            err.to_string().contains("unmanaged"),
            "unexpected error: {err}"
        );
        assert!(
            install_dir.join("user-file.txt").exists(),
            "an unmanaged dir must be left untouched"
        );
    }

    /// The project-local legacy sweep must remove exactly the tracedecay-owned
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
                    "tracedecay": { "command": "tracedecay", "args": ["serve"] },
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
                        { "command": "tracedecay hook-cursor-session-start" },
                        { "command": "./my-hook.sh" }
                    ]
                }
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            cursor_dir.join("rules/tracedecay.mdc"),
            "Prefer tracedecay MCP tools",
        )
        .unwrap();
        std::fs::write(
            cursor_dir.join("permissions.json"),
            serde_json::to_string_pretty(&json!({
                "mcpAllowlist": ["tracedecay:tracedecay_search"]
            }))
            .unwrap(),
        )
        .unwrap();

        sweep_legacy_project_artifacts(project.path()).expect("sweep should succeed");

        let mcp = load_json_file(&cursor_dir.join("mcp.json"));
        assert!(
            mcp["mcpServers"].get("tracedecay").is_none(),
            "project-local MCP entry must be removed"
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
            !cursor_dir.join("rules/tracedecay.mdc").exists(),
            "the legacy steering rule must be removed"
        );
        let permissions = load_json_file(&cursor_dir.join("permissions.json"));
        assert_eq!(
            permissions["mcpAllowlist"],
            json!(["tracedecay:tracedecay_search"]),
            "per-repo permissions.json allowlists are README-endorsed user config"
        );
    }

    /// A project whose `.cursor/` only holds user-authored config (no legacy
    /// tracedecay artifacts) must come through the sweep byte-identical — no
    /// rewrites, no backups, no deletions.
    #[test]
    fn sweep_is_noop_without_legacy_tracedecay_artifacts() {
        let project = TempDir::new().unwrap();
        let cursor_dir = project.path().join(".cursor");
        std::fs::create_dir_all(cursor_dir.join("rules")).unwrap();
        let mcp = serde_json::to_string_pretty(&json!({
            "mcpServers": { "other": { "url": "https://example.com/mcp" } }
        }))
        .unwrap();
        std::fs::write(cursor_dir.join("mcp.json"), &mcp).unwrap();
        // A user file that happens to use the legacy rule filename but not
        // the tracedecay-generated contents stays untouched.
        let rule = "---\ndescription: my own rule\n---\nFollow project conventions.\n";
        std::fs::write(cursor_dir.join("rules/tracedecay.mdc"), rule).unwrap();

        sweep_legacy_project_artifacts(project.path()).expect("sweep should succeed");

        assert_eq!(
            std::fs::read_to_string(cursor_dir.join("mcp.json")).unwrap(),
            mcp
        );
        assert_eq!(
            std::fs::read_to_string(cursor_dir.join("rules/tracedecay.mdc")).unwrap(),
            rule
        );
        let mut files = crate::agents::collect_regular_files(&cursor_dir).unwrap();
        files.sort();
        assert_eq!(
            files,
            vec![
                cursor_dir.join("mcp.json"),
                cursor_dir.join("rules/tracedecay.mdc")
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
    /// tracedecay has not indexed: it must not bootstrap a `.tracedecay/` index
    /// or panic.
    #[tokio::test]
    async fn post_install_does_not_bootstrap_index() {
        let project = tempfile::tempdir().expect("tempdir");
        CursorIntegration.post_install(Some(project.path())).await;
        assert!(
            !project.path().join(".tracedecay").exists(),
            "post_install must not create an index on an unindexed project"
        );
    }

    /// A `None` project path is a no-op and must not panic.
    #[tokio::test]
    async fn post_install_handles_missing_project_path() {
        CursorIntegration.post_install(None).await;
    }
}
