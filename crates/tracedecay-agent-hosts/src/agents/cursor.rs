//! Cursor agent integration.
//!
//! Installs tracedecay's Cursor plugin bundle into Cursor's local plugin
//! directory. The plugin owns MCP, hooks, and rule configuration.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::errors::{Result, TraceDecayError};

use super::host_bundle_v2::{HostBundleComponentV1, HostBundleRegistrationStateV1};
use super::{
    AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext, UpdatePluginOutcome,
    backup_and_write_json, load_json_file, load_jsonc_file_strict, safe_write_text_file,
};

/// Cursor agent.
pub struct CursorIntegration;

/// Model-invocable skills shipped by the Cursor plugin.
///
/// The plugin bundle owns this inventory. Host steering adapters can re-export
/// it without reaching back into a composition-root hook module.
pub const CURSOR_PLUGIN_SKILLS: &[&str] = &[
    "assessing-impact",
    "code-health",
    "diagnosing-analytics",
    "discovering-tracedecay",
    "editing-safely",
    "exploring-code",
    "fixing-build-and-type-errors",
    "inspecting-managed-skills",
    "investigating-unexpected-changes",
    "managing-session-context",
    "managing-work",
    "managing-workflows",
    "project-memory",
    "reviewing-changes",
    "tracing-functions",
    "using-the-cli",
    "using-tracedecay",
];

impl AgentIntegration for CursorIntegration {
    fn name(&self) -> &'static str {
        "Cursor"
    }

    fn id(&self) -> &'static str {
        "cursor"
    }

    fn supports_local_install(&self) -> bool {
        true
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

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mCursor integration\x1b[0m");
        let project_cursor = ctx.project_path.join(".cursor");
        doctor_check_plugin(dc, &ctx.home);
        doctor_check_native_extension(dc, &ctx.home);
        if legacy_project_cursor_has_tracedecay(&project_cursor) {
            dc.warn(
                "legacy project Cursor MCP/hooks/rule files are present; rerun \
                 `tracedecay install --agent cursor` from this project to remove \
                 tracedecay-owned entries",
            );
        }
        super::cursor_diagnostics::report_cursor_mcp_log_findings(dc, &ctx.home);
    }

    fn healthcheck_with_daemon_status(
        &self,
        dc: &mut DoctorCounters,
        ctx: &HealthcheckContext,
        daemon_status: Option<&Value>,
    ) {
        self.healthcheck(dc, ctx);
        doctor_check_session_ingest(dc, &ctx.project_path, daemon_status);
    }

    fn host_component_registration(
        &self,
        component: HostBundleComponentV1,
        ctx: &HealthcheckContext,
    ) -> HostBundleRegistrationStateV1 {
        if component == HostBundleComponentV1::Agent {
            return cursor_native_extension_registration(&ctx.home);
        }
        let plugin_dir = cursor_plugin_install_dir(&ctx.home);
        let manifest_path = cursor_plugin_manifest_path(&ctx.home);
        let Ok(manifest_bytes) = std::fs::read(&manifest_path) else {
            return HostBundleRegistrationStateV1::Missing;
        };
        let Ok(manifest) = serde_json::from_slice::<Value>(&manifest_bytes) else {
            return HostBundleRegistrationStateV1::Corrupt;
        };
        if manifest.get("name").and_then(Value::as_str) != Some("tracedecay") {
            return HostBundleRegistrationStateV1::Corrupt;
        }
        let mcp = load_json_file(&plugin_dir.join("mcp.json"));
        let mcp_current = mcp
            .pointer("/mcpServers/tracedecay")
            .is_some_and(Value::is_object);
        if matches!(
            component,
            HostBundleComponentV1::ContextMcp | HostBundleComponentV1::OperatorMcp
        ) {
            return if mcp_current {
                HostBundleRegistrationStateV1::Current
            } else {
                HostBundleRegistrationStateV1::Repairable
            };
        }
        let hooks = load_json_file(&plugin_dir.join("hooks/hooks.json"));
        let native_hooks_current =
            cursor_plugin_hook_expectations()
                .iter()
                .all(|(event, command)| {
                    hooks["hooks"][event.as_str()]
                        .as_array()
                        .is_some_and(|entries| {
                            entries.iter().any(|entry| {
                                entry["command"]
                                    .as_str()
                                    .is_some_and(|value| value.contains(command))
                            })
                        })
                });
        if native_hooks_current && plugin_dir.join("rules/tracedecay.mdc").is_file() {
            HostBundleRegistrationStateV1::Current
        } else {
            HostBundleRegistrationStateV1::Repairable
        }
    }

    fn is_detected(&self, home: &Path) -> bool {
        home.join(".cursor").is_dir()
    }

    fn primary_config_path(&self, home: &Path) -> Option<std::path::PathBuf> {
        Some(cursor_plugin_manifest_path(home))
    }

    fn host_registration_paths(&self, home: &Path) -> Vec<PathBuf> {
        vec![cursor_plugin_manifest_path(home)]
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        cursor_plugin_manifest_path(home).exists()
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

pub fn cursor_plugin_install_dir(home: &Path) -> PathBuf {
    home.join(".cursor/plugins/local/tracedecay")
}

fn cursor_plugin_manifest_path(home: &Path) -> PathBuf {
    cursor_plugin_install_dir(home).join(".cursor-plugin/plugin.json")
}

/// Deploy directory of the native diagnostics extension, versioned exactly
/// like every VS Code-family extension install (`publisher.name-version`) and
/// stamped with the real release version — a `0.0.0` directory next to
/// otherwise-versioned components was an unstampable literal.
pub(super) fn cursor_native_extension_relative_dir() -> String {
    format!(
        ".cursor/extensions/tracedecay.cursor-native-{}",
        crate::PRODUCT_VERSION
    )
}

fn cursor_native_extension_install_dir(home: &Path) -> PathBuf {
    home.join(cursor_native_extension_relative_dir())
}

fn cursor_native_extension_registration(home: &Path) -> HostBundleRegistrationStateV1 {
    let install_dir = cursor_native_extension_install_dir(home);
    let manifest_path = install_dir.join("package.json");
    let Ok(manifest_bytes) = std::fs::read(&manifest_path) else {
        return HostBundleRegistrationStateV1::Missing;
    };
    let Ok(manifest) = serde_json::from_slice::<Value>(&manifest_bytes) else {
        return HostBundleRegistrationStateV1::Corrupt;
    };
    let expected_manifest = manifest.get("name").and_then(Value::as_str) == Some("cursor-native")
        && manifest.get("publisher").and_then(Value::as_str) == Some("tracedecay")
        && manifest.get("main").and_then(Value::as_str) == Some("./dist/extension.js");
    if !expected_manifest {
        return HostBundleRegistrationStateV1::Corrupt;
    }
    if install_dir.join("dist/extension.js").is_file() {
        HostBundleRegistrationStateV1::Current
    } else {
        HostBundleRegistrationStateV1::Repairable
    }
}

/// Doctor coverage for the deployed native diagnostics extension — the one
/// Cursor component the plugin-dir checks never touched, so a missing or
/// half-deployed extension was invisible. A wholly absent extension is
/// informational (the plugin-only install surface never claims it); a
/// stale-version or half-deployed one warns because an install claimed it
/// and it no longer loads current diagnostics.
fn doctor_check_native_extension(dc: &mut DoctorCounters, home: &Path) {
    let install_dir = cursor_native_extension_install_dir(home);
    match cursor_native_extension_registration(home) {
        HostBundleRegistrationStateV1::Current => dc.pass(&format!(
            "Cursor native diagnostics extension {} deployed at {}",
            crate::PRODUCT_VERSION,
            install_dir.display()
        )),
        HostBundleRegistrationStateV1::Missing => {
            let stale = stale_native_extension_dirs(home);
            if stale.is_empty() {
                dc.info(&format!(
                    "Cursor native diagnostics extension {} not deployed ({}) — run \
                     `tracedecay install --agent cursor`",
                    crate::PRODUCT_VERSION,
                    install_dir.display()
                ));
            } else {
                dc.warn(&format!(
                    "Cursor native diagnostics extension is stale ({}) while {} is current — \
                     run `tracedecay install --agent cursor` to redeploy",
                    stale.join(", "),
                    crate::PRODUCT_VERSION
                ));
            }
        }
        HostBundleRegistrationStateV1::Repairable => dc.warn(&format!(
            "Cursor native diagnostics extension at {} is incomplete (dist/extension.js \
             missing) — run `tracedecay install --agent cursor`",
            install_dir.display()
        )),
        HostBundleRegistrationStateV1::Corrupt => dc.fail(&format!(
            "package.json at {} is not the tracedecay cursor-native extension — inspect and \
             remove it, then run `tracedecay install --agent cursor`",
            install_dir.display()
        )),
    }
}

/// Names of `~/.cursor/extensions/tracedecay.cursor-native-*` directories left
/// by other product versions (e.g. the unstamped `0.0.0` deploys).
fn stale_native_extension_dirs(home: &Path) -> Vec<String> {
    let current = format!("tracedecay.cursor-native-{}", crate::PRODUCT_VERSION);
    let Ok(entries) = std::fs::read_dir(home.join(".cursor/extensions")) else {
        return Vec::new();
    };
    let mut stale: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .filter(|name| name.starts_with("tracedecay.cursor-native-") && *name != current)
        .collect();
    stale.sort();
    stale
}

const RETIRED_CURSOR_MEMORY_RULE_MARKER: &str =
    "<!-- generated by tracedecay from the project fact store; do not edit by hand -->";

fn remove_retired_global_cursor_memory_rule(home: &Path) -> Result<bool> {
    let rule_path = home.join(".cursor/rules/tracedecay-memory.mdc");
    let metadata = match std::fs::symlink_metadata(&rule_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(TraceDecayError::Config {
                message: format!(
                    "failed to inspect retired Cursor memory rule {}: {error}",
                    rule_path.display()
                ),
            });
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Ok(false);
    }
    let contents =
        std::fs::read_to_string(&rule_path).map_err(|error| TraceDecayError::Config {
            message: format!(
                "failed to read retired Cursor memory rule {}: {error}",
                rule_path.display()
            ),
        })?;
    if !contents.contains(RETIRED_CURSOR_MEMORY_RULE_MARKER) {
        return Ok(false);
    }
    std::fs::remove_file(&rule_path).map_err(|error| TraceDecayError::Config {
        message: format!(
            "failed to remove retired Cursor memory rule {}: {error}",
            rule_path.display()
        ),
    })?;
    Ok(true)
}

fn install_cursor_plugin(home: &Path, tracedecay_bin: &str) -> Result<()> {
    remove_retired_global_cursor_memory_rule(home)?;
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
    super::retired_memory_digest::remove_state(&profile_root)?;
    crate::automation::skill_targets::install_managed_skills(
        &profile_root,
        crate::automation::skill_targets::SkillInstallTarget::Cursor,
        install_dir,
    )?;
    Ok(())
}

fn write_embedded_plugin(install_dir: &Path, tracedecay_bin: &str) -> Result<()> {
    for (relative, rendered) in rendered_plugin_files(tracedecay_bin)? {
        safe_write_text_file(&install_dir.join(relative), &rendered, None)?;
    }
    Ok(())
}

/// Canonical rendered Cursor plugin inventory shared by explicit artifact
/// refresh and the receipt-backed first-party catalog.
pub(crate) fn rendered_plugin_files(tracedecay_bin: &str) -> Result<Vec<(&'static str, String)>> {
    embedded_plugin_files()
        .into_iter()
        .map(|(relative, contents)| {
            let rendered = match relative {
                ".cursor-plugin/plugin.json" => {
                    super::plugin_bundle::stamp_manifest_version(contents)?
                }
                "mcp.json" => super::plugin_bundle::set_mcp_command(contents, tracedecay_bin)?,
                "hooks/hooks.json" => cursor_plugin_hooks(contents, tracedecay_bin)?,
                _ => contents.to_string(),
            };
            Ok((relative, rendered))
        })
        .collect()
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
    super::sweep_superseded_plugin_siblings(install_dir, &[".cursor-plugin/plugin.json"])?;
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
    sweep_retired_bundle_skill_dirs(install_dir)?;
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
fn sweep_retired_bundle_skill_dirs(install_dir: &Path) -> Result<()> {
    let skills_root = install_dir.join("skills");
    let entries = match std::fs::read_dir(&skills_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(TraceDecayError::Config {
                message: format!(
                    "failed to inspect retired Cursor plugin skills at {}: {error}",
                    skills_root.display()
                ),
            });
        }
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
    for entry in entries {
        let entry = entry.map_err(|error| TraceDecayError::Config {
            message: format!(
                "failed to inspect retired Cursor plugin skills at {}: {error}",
                skills_root.display()
            ),
        })?;
        if !entry
            .file_type()
            .map_err(|error| TraceDecayError::Config {
                message: format!(
                    "failed to inspect retired Cursor plugin skill at {}: {error}",
                    entry.path().display()
                ),
            })?
            .is_dir()
        {
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
        std::fs::remove_dir_all(entry.path()).map_err(|error| TraceDecayError::Config {
            message: format!(
                "failed to remove retired Cursor plugin skill at {}: {error}",
                entry.path().display()
            ),
        })?;
    }
    Ok(())
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
    // Retired in 0.0.66 when dynamic memory moved to ~/.cursor/rules. Keep the
    // old receipt-owned path in the sweep inventory so install and uninstall
    // can remove artifacts written by older bundles.
    paths.push(install_dir.join("rules/tracedecay-memory.mdc"));
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
        super::cursor_diagnostics::plugin_version_staleness(&manifest, crate::PRODUCT_VERSION)
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

#[derive(serde::Deserialize)]
struct CursorSessionIngestHealth {
    tracked_transcripts: u64,
    pending_transcripts: u64,
    pending_bytes: u64,
    max_transcript_pending_bytes: u64,
}

#[derive(Debug, PartialEq, Eq)]
enum CursorPlaceholderPathsState {
    Available(Vec<String>),
    Unavailable(String),
}

fn cursor_placeholder_paths_state(status: &Value) -> Option<CursorPlaceholderPathsState> {
    let value = status.get("cursor_session_placeholder_paths")?;
    if let Some(paths) = value.as_array() {
        return Some(CursorPlaceholderPathsState::Available(
            paths
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
        ));
    }
    if value.get("status").and_then(Value::as_str) == Some("unavailable") {
        let reason = value
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned();
        return Some(CursorPlaceholderPathsState::Unavailable(reason));
    }
    None
}

/// Flags a stalled Cursor transcript ingest using Doctor's daemon snapshot.
fn doctor_check_session_ingest(
    dc: &mut DoctorCounters,
    project_path: &Path,
    daemon_status: Option<&Value>,
) {
    let Some(status) = daemon_status else {
        return;
    };
    let placeholder_paths = cursor_placeholder_paths_state(status);
    if let Some(CursorPlaceholderPathsState::Unavailable(reason)) = &placeholder_paths {
        dc.warn(&format!(
            "Cursor transcript placeholder-path diagnostics unavailable from daemon ({reason}); \
             literal workspace placeholders could not be checked"
        ));
    }
    if status
        .pointer("/cursor_session_ingest/status")
        .and_then(Value::as_str)
        == Some("unavailable")
    {
        dc.warn("Cursor transcript ingest health unavailable from daemon session authority");
        return;
    }
    let Some(health) = status
        .get("cursor_session_ingest")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
    else {
        return;
    };
    let paths = match placeholder_paths {
        Some(CursorPlaceholderPathsState::Available(paths)) => paths,
        Some(CursorPlaceholderPathsState::Unavailable(_)) | None => Vec::new(),
    };
    report_cursor_session_ingest(dc, project_path, &health, paths.iter().map(String::as_str));
}

fn report_cursor_session_ingest<'a>(
    dc: &mut DoctorCounters,
    project_path: &Path,
    health: &CursorSessionIngestHealth,
    placeholder_paths: impl Iterator<Item = &'a str>,
) {
    let placeholder_paths = placeholder_paths.collect::<Vec<_>>();
    if !placeholder_paths.is_empty() {
        dc.warn(&format!(
            "Cursor transcript ingest has {} path(s) with a literal workspace placeholder; \
             Cursor did not expand `${{workspaceFolder}}`, so session recall will miss those transcripts",
            placeholder_paths.len(),
        ));
        for path in placeholder_paths {
            dc.info(&format!("  - {path}"));
        }
    }
    if health.max_transcript_pending_bytes
        > crate::ports::hook_runtime::cursor_catch_up_ingest_max_bytes()
    {
        dc.warn(&format!(
            "Cursor transcript ingest looks stalled: a transcript has {} un-ingested \
             byte(s) ({} byte(s) total across {} transcript(s)), exceeding the {} byte \
             per-transcript hook catch-up cap — it will not drain automatically and \
             session recall is missing those turns. Run `tracedecay sessions import \
             --project-path {}` to schedule bounded convergence",
            health.max_transcript_pending_bytes,
            health.pending_bytes,
            health.pending_transcripts,
            crate::ports::hook_runtime::cursor_catch_up_ingest_max_bytes(),
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
    use crate::agents::host_bundle_v2::{
        HostBundleComponentDoctorStateV1, HostBundleError, HostBundleLifecycleOpV1,
        HostBundleRegistrationStateV1, HostBundleWriterV1, HostComponentSetExecutionRequestV1,
        HostComponentSetLifecycleRequestV1, HostComponentSetTransactionV1, HostKindV1,
    };
    use tempfile::TempDir;
    use tracedecay_host_integration::HostCapabilityUnavailableReasonV1;

    const RETIRED_CURSOR_MEMORY_RULE_FIXTURE: &str =
        "<!-- generated by tracedecay from the project fact store; do not edit by hand -->";

    fn plugin_source_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugin")
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
            8,
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

    #[test]
    fn clean_cursor_install_update_and_doctor_preserve_user_config() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        let user_config = home.path().join(".cursor/mcp.json");
        std::fs::create_dir_all(user_config.parent().unwrap()).unwrap();
        let user_config_bytes = br#"{"mcpServers":{"operator":{"command":"other"}}}"#;
        std::fs::write(&user_config, user_config_bytes).unwrap();

        let integration = CursorIntegration;
        let context = InstallContext {
            home: home.path().to_path_buf(),
            tracedecay_bin: "/opt/tracedecay-previous".to_string(),
            tool_permissions: super::super::expected_tool_perms(),
            project_root: None,
            dashboard: true,
        };
        assert_eq!(
            integration.update_plugin(&context).unwrap(),
            UpdatePluginOutcome::NotInstalled,
            "update must refuse to create a Cursor installation that was never installed"
        );
        assert!(!cursor_plugin_install_dir(home.path()).exists());

        install_cursor_plugin(home.path(), &context.tracedecay_bin).unwrap();
        let plugin_dir = cursor_plugin_install_dir(home.path());
        assert!(plugin_dir.join("agents/code-explorer.md").is_file());
        let mcp_path = plugin_dir.join("mcp.json");
        let installed_mcp: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&mcp_path).unwrap()).unwrap();
        assert_eq!(
            installed_mcp["mcpServers"]["tracedecay"]["command"],
            "/opt/tracedecay-previous"
        );

        let updated_context = InstallContext {
            tracedecay_bin: "/opt/tracedecay-next".to_string(),
            ..context
        };
        assert!(matches!(
            integration.update_plugin(&updated_context).unwrap(),
            UpdatePluginOutcome::Refreshed(paths) if paths == vec![plugin_dir.clone()]
        ));
        let updated_mcp: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&mcp_path).unwrap()).unwrap();
        assert_eq!(
            updated_mcp["mcpServers"]["tracedecay"]["command"], "/opt/tracedecay-next",
            "the in-composer MCP registration must refresh for the new binary"
        );
        assert!(plugin_dir.join("agents/code-explorer.md").is_file());
        assert_eq!(std::fs::read(&user_config).unwrap(), user_config_bytes);

        let before_idempotent_update = std::fs::read(&mcp_path).unwrap();
        assert!(matches!(
            integration.update_plugin(&updated_context).unwrap(),
            UpdatePluginOutcome::Refreshed(paths) if paths == vec![plugin_dir.clone()]
        ));
        assert_eq!(std::fs::read(&mcp_path).unwrap(), before_idempotent_update);

        let mut doctor = DoctorCounters::new();
        integration.healthcheck(
            &mut doctor,
            &HealthcheckContext {
                home: home.path().to_path_buf(),
                project_path: project.path().to_path_buf(),
            },
        );
        assert_eq!(doctor.issues, 0);
        assert_eq!(doctor.warnings, 0);
    }

    fn cursor_component_set(
        tracedecay_bin: &str,
    ) -> crate::agents::host_bundle_registry::VerifiedEmbeddedHostComponentSetV1 {
        use crate::agents::host_bundle_registry::{
            default_components, verified_embedded_host_component_set_with_tracedecay_bin,
        };

        verified_embedded_host_component_set_with_tracedecay_bin(
            HostKindV1::CursorDesktop,
            &default_components(HostKindV1::CursorDesktop),
            0,
            tracedecay_bin,
        )
        .expect("the packaged Cursor Desktop component set must verify")
    }

    fn cursor_component_request(
        operation: HostBundleLifecycleOpV1,
        operation_id: [u8; 16],
        explicit_confirmation: bool,
    ) -> HostComponentSetExecutionRequestV1 {
        HostComponentSetExecutionRequestV1 {
            lifecycle: HostComponentSetLifecycleRequestV1 {
                operation,
                expected_host: HostKindV1::CursorDesktop,
                expected_components: crate::agents::host_bundle_registry::default_components(
                    HostKindV1::CursorDesktop,
                ),
                explicit_confirmation,
                hermes_profile_bindings: 0,
            },
            operation_id,
        }
    }

    #[test]
    fn cursor_component_transaction_updates_doctor_and_preserves_denied_state() {
        use crate::agents::host_bundle_registry::{
            HostBundleRegistryError, verified_embedded_default_host_component_set,
        };

        let home = TempDir::new().unwrap();
        let lifecycle = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        let user_config = home.path().join(".cursor/mcp.json");
        let user_config_bytes = br#"{"mcpServers":{"operator":{"command":"other"}}}"#;
        std::fs::create_dir_all(user_config.parent().unwrap()).unwrap();
        std::fs::write(&user_config, user_config_bytes).unwrap();

        let previous = cursor_component_set("/opt/tracedecay-v1");
        let install = cursor_component_request(HostBundleLifecycleOpV1::Install, [61; 16], true);
        let mut writer =
            HostBundleWriterV1::open_with_lifecycle_root(home.path(), lifecycle.path()).unwrap();
        let mut registration = crate::agents::host_component_registration::CatalogHostComponentRegistrationAuthority::new_with_tracedecay_bin(
            "cursor",
            home.path(),
            lifecycle.path(),
            install.lifecycle.operation,
            "/opt/tracedecay-v1".to_string(),
        )
        .unwrap();
        HostComponentSetTransactionV1::new(&mut writer)
            .execute(
                &previous.component_set,
                &install,
                &previous,
                &mut registration,
            )
            .expect("the packaged Cursor set must install through its production transaction");

        let mcp_path = cursor_plugin_install_dir(home.path()).join("mcp.json");
        let installed_mcp: Value =
            serde_json::from_slice(&std::fs::read(&mcp_path).unwrap()).unwrap();
        assert_eq!(
            installed_mcp["mcpServers"]["tracedecay"]["command"],
            "/opt/tracedecay-v1"
        );

        let current_bin =
            super::super::which_tracedecay().unwrap_or_else(|| "tracedecay".to_string());
        let current = cursor_component_set(&current_bin);
        let update = cursor_component_request(HostBundleLifecycleOpV1::Update, [62; 16], true);
        let mut registration = crate::agents::host_component_registration::CatalogHostComponentRegistrationAuthority::new_with_tracedecay_bin(
            "cursor",
            home.path(),
            lifecycle.path(),
            update.lifecycle.operation,
            current_bin.clone(),
        )
        .unwrap();
        let update_receipt = HostComponentSetTransactionV1::new(&mut writer)
            .execute(&current.component_set, &update, &current, &mut registration)
            .expect("a newer packaged Cursor set must update through the transaction");
        assert_eq!(update_receipt.operation_id, update.operation_id);

        let updated_mcp: Value =
            serde_json::from_slice(&std::fs::read(&mcp_path).unwrap()).unwrap();
        assert_eq!(
            updated_mcp["mcpServers"]["tracedecay"]["command"],
            current_bin
        );
        let updated_bytes = std::fs::read(&mcp_path).unwrap();

        let mut repeat_registration = crate::agents::host_component_registration::CatalogHostComponentRegistrationAuthority::new_with_tracedecay_bin(
            "cursor",
            home.path(),
            lifecycle.path(),
            update.lifecycle.operation,
            current_bin.clone(),
        )
        .unwrap();
        let repeated_receipt = HostComponentSetTransactionV1::new(&mut writer)
            .execute(
                &current.component_set,
                &update,
                &current,
                &mut repeat_registration,
            )
            .expect("repeating the same confirmed transaction must be idempotent");
        assert_eq!(repeated_receipt, update_receipt);
        assert_eq!(std::fs::read(&mcp_path).unwrap(), updated_bytes);

        let denied = cursor_component_request(HostBundleLifecycleOpV1::Update, [63; 16], false);
        let rejected = cursor_component_set("/opt/tracedecay-v3");
        let mut denied_registration = crate::agents::host_component_registration::CatalogHostComponentRegistrationAuthority::new_with_tracedecay_bin(
            "cursor",
            home.path(),
            lifecycle.path(),
            denied.lifecycle.operation,
            "/opt/tracedecay-v3".to_string(),
        )
        .unwrap();
        let preview = HostComponentSetTransactionV1::new(&mut writer)
            .preview(
                &rejected.component_set,
                &denied,
                &rejected,
                &mut denied_registration,
            )
            .expect("a denied Cursor update must still produce its truthful preview");
        assert_eq!(
            HostComponentSetTransactionV1::new(&mut writer)
                .execute_confirmed(
                    &rejected.component_set,
                    &denied,
                    &preview,
                    &rejected,
                    &mut denied_registration,
                )
                .expect_err("an unconfirmed Cursor update must not mutate the installed release"),
            HostBundleError::ConfirmationRequired
        );
        assert_eq!(std::fs::read(&mcp_path).unwrap(), updated_bytes);

        let report = crate::agents::inspect_receipt_backed_host_components(
            &HealthcheckContext {
                home: home.path().to_path_buf(),
                project_path: project.path().to_path_buf(),
            },
            lifecycle.path(),
        )
        .expect("Doctor must inspect the transaction receipts through production registration");
        assert_eq!(
            report.components.len(),
            current.component_set.components.len(),
            "Doctor must report exactly the Cursor Desktop component set: {report:#?}"
        );
        for expected_component in [
            HostBundleComponentV1::Core,
            HostBundleComponentV1::Agent,
            HostBundleComponentV1::ContextMcp,
        ] {
            let component = report
                .components
                .iter()
                .find(|component| component.component == Some(expected_component))
                .unwrap_or_else(|| {
                    panic!("Doctor omitted Cursor Desktop {expected_component:?}: {report:#?}")
                });
            assert_eq!(
                component.host,
                Some(HostKindV1::CursorDesktop),
                "unexpected Doctor host for {expected_component:?}: {component:#?}; full report: {report:#?}"
            );
            assert_eq!(
                component.state,
                HostBundleComponentDoctorStateV1::Current,
                "unexpected Doctor state for {expected_component:?}: {component:#?}; full report: {report:#?}"
            );
            assert_eq!(
                component.registration,
                Some(HostBundleRegistrationStateV1::Current),
                "unexpected registration state for {expected_component:?}: {component:#?}; full report: {report:#?}"
            );
        }
        assert_eq!(std::fs::read(&user_config).unwrap(), user_config_bytes);

        assert_eq!(
            verified_embedded_default_host_component_set(HostKindV1::CursorCloud, 0),
            Err(HostBundleRegistryError::HostComponentSetUnavailable {
                host: HostKindV1::CursorCloud,
                reason: HostCapabilityUnavailableReasonV1::HostRegistrationUnsupported,
            }),
            "Cursor Cloud remains excluded from the production component transaction"
        );
    }

    #[test]
    fn native_extension_registration_is_receipt_doctor_ready() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(
            cursor_native_extension_registration(tmp.path()),
            HostBundleRegistrationStateV1::Missing
        );

        let install_dir = cursor_native_extension_install_dir(tmp.path());
        std::fs::create_dir_all(install_dir.join("dist")).unwrap();
        std::fs::write(
            install_dir.join("package.json"),
            r#"{
                "name": "cursor-native",
                "publisher": "tracedecay",
                "main": "./dist/extension.js"
            }"#,
        )
        .unwrap();
        assert_eq!(
            cursor_native_extension_registration(tmp.path()),
            HostBundleRegistrationStateV1::Repairable
        );

        std::fs::write(
            install_dir.join("dist/extension.js"),
            "module.exports = {};",
        )
        .unwrap();
        assert_eq!(
            cursor_native_extension_registration(tmp.path()),
            HostBundleRegistrationStateV1::Current
        );
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
        ] {
            assert!(
                deploy.contains(expected),
                "Cursor deploy set is missing {expected}"
            );
        }
        assert!(
            !deploy.contains("rules/tracedecay-memory.mdc"),
            "dynamic project memory must not modify receipt-owned plugin files"
        );
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
        let mut listed: Vec<String> = CURSOR_PLUGIN_SKILLS
            .iter()
            .map(|skill| (*skill).to_string())
            .collect();
        listed.sort();
        assert_eq!(
            bundled, listed,
            "hooks::CURSOR_PLUGIN_SKILLS must list exactly the model-invocable bundled skills"
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

    #[test]
    fn uninstall_sweeps_retired_plugin_memory_rule() {
        let tmp = TempDir::new().unwrap();
        let install_dir = tmp.path().join("tracedecay");
        write_embedded_plugin(&install_dir, "tracedecay").expect("embedded install should succeed");
        let retired_rule = install_dir.join("rules/tracedecay-memory.mdc");
        std::fs::write(&retired_rule, RETIRED_CURSOR_MEMORY_RULE_FIXTURE).unwrap();

        remove_cursor_plugin_install(&install_dir).expect("uninstall should succeed");

        assert!(
            !retired_rule.exists(),
            "uninstall must remove the memory rule retired from the plugin inventory"
        );
        assert!(
            !install_dir.exists(),
            "the retired managed rule must not strand the owned plugin directory"
        );
    }

    #[test]
    fn install_sweeps_retired_plugin_memory_rule() {
        let home = TempDir::new().unwrap();
        let install_dir = cursor_plugin_install_dir(home.path());
        write_embedded_plugin(&install_dir, "old-tracedecay")
            .expect("old embedded install should succeed");
        let retired_rule = install_dir.join("rules/tracedecay-memory.mdc");
        std::fs::write(&retired_rule, RETIRED_CURSOR_MEMORY_RULE_FIXTURE).unwrap();

        install_cursor_plugin(home.path(), "new-tracedecay").expect("install should refresh");

        assert!(
            !retired_rule.exists(),
            "install must remove the memory rule retired from the plugin inventory"
        );
        assert!(install_dir.join("rules/tracedecay.mdc").exists());
    }

    #[test]
    fn retired_global_memory_rule_is_removed_only_when_tracedecay_managed() {
        let home = TempDir::new().unwrap();
        let rule = home.path().join(".cursor/rules/tracedecay-memory.mdc");
        std::fs::create_dir_all(rule.parent().unwrap()).unwrap();
        std::fs::write(
            &rule,
            format!("{RETIRED_CURSOR_MEMORY_RULE_MARKER}\nmanaged memory"),
        )
        .unwrap();

        assert!(remove_retired_global_cursor_memory_rule(home.path()).unwrap());
        assert!(!rule.exists());

        std::fs::write(&rule, "my own Cursor rule").unwrap();
        assert!(!remove_retired_global_cursor_memory_rule(home.path()).unwrap());
        assert_eq!(
            std::fs::read_to_string(&rule).unwrap(),
            "my own Cursor rule"
        );
    }

    #[test]
    fn install_sweeps_owned_superseded_plugin_siblings_only() {
        let home = TempDir::new().unwrap();
        let plugins = home.path().join(".cursor/plugins/local");
        let retired = plugins.join("tracedecay.pre-v2-adopt");
        let foreign = plugins.join("tracedecay.personal");
        for dir in [&retired, &foreign] {
            std::fs::create_dir_all(dir.join(".cursor-plugin")).unwrap();
            std::fs::write(
                dir.join(".cursor-plugin/plugin.json"),
                serde_json::to_vec(&json!({ "name": "tracedecay" })).unwrap(),
            )
            .unwrap();
        }

        install_cursor_plugin(home.path(), "tracedecay").expect("install should succeed");

        assert!(
            !retired.exists(),
            "a manifest-owned superseded tracedecay sibling must be swept"
        );
        assert!(
            foreign.exists(),
            "an owned-looking sibling without an explicitly retired suffix must be preserved"
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

    fn session_ingest_status(placeholder_paths: Value) -> Value {
        json!({
            "cursor_session_ingest": {
                "tracked_transcripts": 1,
                "pending_transcripts": 0,
                "pending_bytes": 0,
                "max_transcript_pending_bytes": 0,
            },
            "cursor_session_placeholder_paths": placeholder_paths,
        })
    }

    #[test]
    fn cursor_placeholder_paths_empty_array_remains_available() {
        let status = session_ingest_status(json!([]));
        assert_eq!(
            cursor_placeholder_paths_state(&status),
            Some(CursorPlaceholderPathsState::Available(Vec::new()))
        );

        let mut counters = DoctorCounters::new();
        doctor_check_session_ingest(&mut counters, Path::new("/project"), Some(&status));
        assert_eq!(counters.warnings, 0);
    }

    #[test]
    fn cursor_placeholder_paths_nonempty_array_remains_available() {
        let status = session_ingest_status(json!(["${workspaceFolder}/cursor.jsonl"]));
        assert_eq!(
            cursor_placeholder_paths_state(&status),
            Some(CursorPlaceholderPathsState::Available(vec![
                "${workspaceFolder}/cursor.jsonl".to_owned()
            ]))
        );

        let mut counters = DoctorCounters::new();
        doctor_check_session_ingest(&mut counters, Path::new("/project"), Some(&status));
        assert_eq!(counters.warnings, 1);
    }

    #[test]
    fn cursor_placeholder_paths_typed_unavailable_is_warned() {
        let status = session_ingest_status(json!({
            "status": "unavailable",
            "reason": "cursor_session_placeholder_paths_query_failed",
        }));
        assert_eq!(
            cursor_placeholder_paths_state(&status),
            Some(CursorPlaceholderPathsState::Unavailable(
                "cursor_session_placeholder_paths_query_failed".to_owned()
            ))
        );

        let mut counters = DoctorCounters::new();
        doctor_check_session_ingest(&mut counters, Path::new("/project"), Some(&status));
        assert_eq!(counters.issues, 0);
        assert_eq!(counters.warnings, 1);
    }

    #[test]
    fn session_ingest_healthcheck_reports_daemon_snapshot() {
        // Register a finite ceiling: the unwired default is `u64::MAX`, which
        // both overflows the `+ 1` below and means the over-ceiling warning
        // path could never be exercised.
        crate::ports::hook_runtime::register_cursor_catch_up_ingest_max_bytes(|| 8 * 1024 * 1024);
        let mut counters = DoctorCounters::new();
        let health = CursorSessionIngestHealth {
            tracked_transcripts: 2,
            pending_transcripts: 1,
            pending_bytes: crate::ports::hook_runtime::cursor_catch_up_ingest_max_bytes() + 1,
            max_transcript_pending_bytes:
                crate::ports::hook_runtime::cursor_catch_up_ingest_max_bytes() + 1,
        };

        report_cursor_session_ingest(
            &mut counters,
            Path::new("/project"),
            &health,
            ["${workspaceFolder}/cursor.jsonl"].into_iter(),
        );

        assert_eq!(counters.issues, 0);
        assert_eq!(counters.warnings, 2);
    }

    #[test]
    fn session_ingest_healthcheck_warns_when_daemon_authority_is_unavailable() {
        let mut counters = DoctorCounters::new();
        doctor_check_session_ingest(
            &mut counters,
            Path::new("/project"),
            Some(&serde_json::json!({
                "cursor_session_ingest": {
                    "status": "unavailable",
                    "message": "daemon project session authority is unavailable",
                }
            })),
        );

        assert_eq!(counters.issues, 0);
        assert_eq!(counters.warnings, 1);
    }
}
