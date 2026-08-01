// Rust guideline compliant 2025-10-17
//! Kimi Code CLI agent integration.
//!
//! Kimi Code currently exposes plugin lifecycle only through its interactive
//! `/plugins` host API. `TraceDecay`'s first-party bundle contains the complete
//! managed plugin artifact set and may reap its own superseded managed trees,
//! while registration in `plugins/installed.json` remains owned by Kimi's
//! interactive host flow. Until Kimi ships a documented non-interactive
//! mutation API, global install/update/uninstall return an explicit
//! remediation instead of mutating the current registration. Project-local `--local`
//! installs write
//! `<project>/.kimi-code/mcp.json` plus prompt rules in `<project>/AGENTS.md`.
//!
//! The pre-plugin Kimi CLI surface (`~/.kimi/mcp.json` + `~/.kimi/AGENTS.md`)
//! is no longer written; `uninstall` and `has_tracedecay` keep reading it as
//! a migration shim so installs written by older tracedecay versions are
//! still cleaned up and noticed by upgrade tracking.

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::errors::{Result, TraceDecayError};

use super::{
    AgentIntegration, DeferredUserAction, DoctorCounters, HealthcheckContext, InstallContext,
    NonInteractiveInstallOutcome, UpdatePluginOutcome, backup_and_write_json, backup_config_file,
    load_json_file, load_json_file_strict, safe_write_json_file, safe_write_text_file,
};

use super::prompt_rules::{PROMPT_RULE_MARKER, PromptRulesOptions};

/// Environment variable that overrides the Kimi Code CLI home directory.
/// When unset, the home resolves to `~/.kimi-code`.
pub const KIMI_CODE_HOME_ENV: &str = "KIMI_CODE_HOME";

/// Plugin id read from Kimi Code CLI's official installed-plugin state.
const KIMI_PLUGIN_ID: &str = "tracedecay";

/// Deploy-relative path of the Kimi Code CLI plugin manifest inside the
/// managed plugin dir (the only bundle entry rendered at install time).
const KIMI_PLUGIN_MANIFEST_RELATIVE: &str = ".kimi-plugin/plugin.json";

/// Kimi Code CLI agent (`tracedecay install --agent kimi`).
pub struct KimiIntegration;

impl AgentIntegration for KimiIntegration {
    fn name(&self) -> &'static str {
        "Kimi CLI"
    }

    fn id(&self) -> &'static str {
        "kimi"
    }

    fn install(&self, ctx: &InstallContext) -> Result<()> {
        let deferred = stage_kimi_install_action(ctx)?;
        Err(deferred_user_action_error(deferred))
    }

    fn preflight_non_interactive_install(
        &self,
        _ctx: &InstallContext,
    ) -> Result<NonInteractiveInstallOutcome> {
        Ok(NonInteractiveInstallOutcome::DeferredUserAction(
            kimi_official_lifecycle_unavailable("install", None),
        ))
    }

    fn prepare_non_interactive_install(
        &self,
        ctx: &InstallContext,
    ) -> Result<NonInteractiveInstallOutcome> {
        stage_kimi_install_action(ctx).map(NonInteractiveInstallOutcome::DeferredUserAction)
    }

    fn interactive_activation_guidance(&self) -> Option<String> {
        Some(kimi_official_lifecycle_unavailable("install", None).remediation)
    }

    fn supports_local_install(&self) -> bool {
        true
    }

    fn install_local(&self, ctx: &InstallContext, project_path: &Path) -> Result<()> {
        let mcp_path = project_path.join(".kimi-code/mcp.json");
        let agents_md = project_path.join("AGENTS.md");
        super::ensure_project_local_safe_paths(
            project_path,
            [mcp_path.as_path(), agents_md.as_path()],
        )?;
        std::fs::create_dir_all(project_path.join(".kimi-code")).ok();
        install_mcp_server(&mcp_path, &ctx.tracedecay_bin)?;
        install_prompt_rules(&agents_md)?;
        super::install_managed_skill_prompt_index(
            &ctx.home,
            &agents_md,
            crate::automation::skill_targets::SkillInstallTarget::Kimi,
        )
    }

    fn uninstall_local(&self, ctx: &InstallContext, project_path: &Path) -> Result<()> {
        let mcp_path = project_path.join(".kimi-code/mcp.json");
        uninstall_mcp_server(&mcp_path);
        let agents_md = project_path.join("AGENTS.md");
        super::remove_managed_skill_prompt_index(
            &ctx.home,
            &agents_md,
            crate::automation::skill_targets::SkillInstallTarget::Kimi,
        )?;
        uninstall_prompt_rules(&agents_md);
        Ok(())
    }

    fn update_plugin(&self, ctx: &InstallContext) -> Result<UpdatePluginOutcome> {
        let code_home = kimi_code_home(&ctx.home);
        if !installed_json_has_tracedecay(&code_home) {
            return Ok(UpdatePluginOutcome::NotInstalled);
        }
        stage_kimi_install_action(ctx).map(UpdatePluginOutcome::DeferredUserAction)
    }

    fn uninstall(&self, ctx: &InstallContext) -> Result<()> {
        let code_home = kimi_code_home(&ctx.home);
        sweep_superseded_kimi_plugins(&code_home)?;
        if installed_json_has_tracedecay(&code_home) {
            return Err(deferred_user_action_error(
                kimi_official_lifecycle_unavailable("remove", None),
            ));
        }

        // Migration shim: pre-plugin tracedecay versions wrote a legacy global
        // install under `~/.kimi` (mcp.json registration, AGENTS.md prompt
        // rules, and the managed skill prompt index). Current installs never
        // write those files; keep removing them here so an uninstall after an
        // upgrade still cleans up the old surface.
        let kimi_dir = ctx.home.join(".kimi");
        let mcp_path = kimi_dir.join("mcp.json");
        uninstall_mcp_server(&mcp_path);

        let agents_md = kimi_dir.join("AGENTS.md");
        super::remove_managed_skill_prompt_index(
            &ctx.home,
            &agents_md,
            crate::automation::skill_targets::SkillInstallTarget::Kimi,
        )?;
        uninstall_prompt_rules(&agents_md);

        eprintln!();
        eprintln!("Uninstall complete. No Kimi Code plugin registration was present.");
        Ok(())
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mKimi CLI integration\x1b[0m");
        doctor_check_plugin(dc, &kimi_code_home(&ctx.home));
    }

    fn host_component_registration(
        &self,
        component: super::host_bundle_v2::HostBundleComponentV1,
        ctx: &HealthcheckContext,
    ) -> super::host_bundle_v2::HostBundleRegistrationStateV1 {
        use super::host_bundle_v2::{
            HostBundleComponentV1, HostBundleRegistrationStateV1 as State,
        };

        let code_home = kimi_code_home(&ctx.home);
        let installed_path = kimi_installed_json_path(&code_home);
        let Ok(installed_bytes) = std::fs::read(&installed_path) else {
            return State::Missing;
        };
        let Ok(installed) = serde_json::from_slice::<serde_json::Value>(&installed_bytes) else {
            return State::Corrupt;
        };
        let Some(entry) = kimi_installed_entry(&installed) else {
            return State::Missing;
        };
        let managed_dir = kimi_plugin_managed_dir(&code_home);
        let expected_root = managed_dir
            .canonicalize()
            .unwrap_or_else(|_| managed_dir.clone());
        let manager_state_current = entry.get("enabled").and_then(serde_json::Value::as_bool)
            != Some(false)
            && entry.get("source").and_then(serde_json::Value::as_str) == Some("local-path")
            && entry
                .get("root")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|root| Path::new(root) == expected_root);
        if !manager_state_current {
            return State::Repairable;
        }
        let manifest_path = managed_dir.join(KIMI_PLUGIN_MANIFEST_RELATIVE);
        let Ok(manifest_bytes) = std::fs::read(&manifest_path) else {
            return State::Repairable;
        };
        let Ok(manifest) = serde_json::from_slice::<serde_json::Value>(&manifest_bytes) else {
            return State::Corrupt;
        };
        if manifest.get("name").and_then(serde_json::Value::as_str) != Some(KIMI_PLUGIN_ID) {
            return State::Corrupt;
        }
        let mcp_current = manifest
            .pointer("/mcpServers/tracedecay")
            .is_some_and(serde_json::Value::is_object);
        if matches!(
            component,
            HostBundleComponentV1::ContextMcp | HostBundleComponentV1::OperatorMcp
        ) {
            return State::Missing;
        }
        let events = manifest
            .get("hooks")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|hook| hook.get("event").and_then(serde_json::Value::as_str))
            .collect::<std::collections::BTreeSet<_>>();
        if mcp_current && events.contains("PostToolUse") && events.contains("Stop") {
            State::Current
        } else {
            State::Repairable
        }
    }

    fn is_detected(&self, home: &Path) -> bool {
        kimi_code_home(home).is_dir()
    }

    fn primary_config_path(&self, home: &Path) -> Option<std::path::PathBuf> {
        Some(kimi_installed_json_path(&kimi_code_home(home)))
    }

    fn activate_deployed_host_registration(&self, ctx: &InstallContext) -> Result<()> {
        let code_home = kimi_code_home(&ctx.home);
        if installed_json_has_tracedecay(&code_home) {
            Ok(())
        } else {
            Err(deferred_user_action_error(
                kimi_official_lifecycle_unavailable("install", None),
            ))
        }
    }

    fn deactivate_deployed_host_registration(&self, ctx: &InstallContext) -> Result<()> {
        let code_home = kimi_code_home(&ctx.home);
        if installed_json_has_tracedecay(&code_home) {
            Err(deferred_user_action_error(
                kimi_official_lifecycle_unavailable("remove", None),
            ))
        } else {
            Ok(())
        }
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        // Migration shim: the legacy `~/.kimi/mcp.json` branch exists only so
        // upgrade tracking still notices installs written before the plugin
        // became the global surface.
        let mcp_path = home.join(".kimi/mcp.json");
        if mcp_path.exists() {
            let json = load_json_file(&mcp_path);
            let servers = json.get("mcpServers");
            if servers.and_then(|v| v.get("tracedecay")).is_some() {
                return true;
            }
        }
        installed_json_has_tracedecay(&kimi_code_home(home))
    }

    fn export_managed_skills_local(
        &self,
        project_root: &Path,
        profile_root: &Path,
    ) -> Result<Vec<crate::automation::skill_targets::SkillInstallSummary>> {
        let agents_md = project_root.join("AGENTS.md");
        if !local_mcp_has_tracedecay(project_root) || !agents_md.exists() {
            return Ok(Vec::new());
        }
        Ok(vec![
            crate::automation::skill_targets::install_managed_skills(
                profile_root,
                crate::automation::skill_targets::SkillInstallTarget::Kimi,
                &agents_md,
            )?,
        ])
    }
}

fn local_mcp_has_tracedecay(project_root: &Path) -> bool {
    let mcp_path = project_root.join(".kimi-code/mcp.json");
    if !mcp_path.exists() {
        return false;
    }
    let json = load_json_file(&mcp_path);
    json.get("mcpServers")
        .and_then(|servers| servers.get("tracedecay"))
        .is_some()
}

// ---------------------------------------------------------------------------
// Kimi Code CLI native plugin helpers
// ---------------------------------------------------------------------------

/// Resolve the Kimi Code CLI home: `$KIMI_CODE_HOME` when set (and non-empty),
/// else `~/.kimi-code` under the install context's home.
fn kimi_code_home(home: &Path) -> PathBuf {
    std::env::var_os(KIMI_CODE_HOME_ENV)
        .filter(|value| !value.is_empty())
        .map_or_else(|| home.join(".kimi-code"), PathBuf::from)
}

/// The managed plugin deploy dir: `<kimi-code-home>/plugins/managed/tracedecay`.
fn kimi_plugin_managed_dir(kimi_code_home: &Path) -> PathBuf {
    kimi_code_home.join("plugins/managed").join(KIMI_PLUGIN_ID)
}

/// Kimi Code CLI's plugin registry: `<kimi-code-home>/plugins/installed.json`.
fn kimi_installed_json_path(kimi_code_home: &Path) -> PathBuf {
    kimi_code_home.join("plugins/installed.json")
}

/// The tracedecay entry inside a parsed `installed.json`, if present.
fn kimi_installed_entry(installed: &serde_json::Value) -> Option<&serde_json::Value> {
    installed
        .get("plugins")
        .and_then(|value| value.as_array())
        .and_then(|plugins| {
            plugins.iter().find(|entry| {
                entry.get("id").and_then(|value| value.as_str()) == Some(KIMI_PLUGIN_ID)
            })
        })
}

/// True when `<kimi-code-home>/plugins/installed.json` registers tracedecay.
fn installed_json_has_tracedecay(kimi_code_home: &Path) -> bool {
    let installed_path = kimi_installed_json_path(kimi_code_home);
    installed_path.exists() && kimi_installed_entry(&load_json_file(&installed_path)).is_some()
}

/// Canonical rendered Kimi Code plugin inventory. The legacy installer and the
/// receipt-backed first-party host-bundle catalog must produce byte-identical
/// files: the component-set transaction verifies installed artifact digests
/// after the compatibility registration adapter re-runs this installer, so any
/// rendering drift between the two writers fails installs with
/// `ArtifactContentMismatch`.
pub(crate) fn rendered_plugin_files(tracedecay_bin: &str) -> Result<Vec<(&'static str, String)>> {
    super::plugin_bundle::kimi_files()
        .into_iter()
        .map(|(relative, contents)| {
            let rendered = if relative == KIMI_PLUGIN_MANIFEST_RELATIVE {
                let stamped = super::plugin_bundle::stamp_manifest_version(contents)?;
                let with_mcp = super::plugin_bundle::set_mcp_command(&stamped, tracedecay_bin)?;
                render_kimi_hook_commands(&with_mcp, tracedecay_bin)?
            } else {
                contents.to_string()
            };
            Ok((relative, rendered))
        })
        .collect()
}

fn deploy_kimi_plugin_to(managed_dir: &Path, tracedecay_bin: &str) -> Result<PathBuf> {
    for (relative, rendered) in rendered_plugin_files(tracedecay_bin)? {
        safe_write_text_file(&managed_dir.join(relative), &rendered, None)?;
    }
    eprintln!(
        "\x1b[32m✔\x1b[0m Installed Kimi Code CLI plugin at {}",
        managed_dir.display()
    );
    Ok(managed_dir.to_path_buf())
}

fn stage_kimi_install_action(ctx: &InstallContext) -> Result<DeferredUserAction> {
    sweep_superseded_kimi_plugins(&kimi_code_home(&ctx.home))?;
    let staged_dir = ctx
        .home
        .join(".tracedecay/host-bundle-stage/kimi/tracedecay");
    deploy_kimi_plugin_to(&staged_dir, &ctx.tracedecay_bin)?;
    Ok(kimi_official_lifecycle_unavailable(
        "install",
        Some(&staged_dir),
    ))
}

fn sweep_superseded_kimi_plugins(kimi_code_home: &Path) -> Result<()> {
    super::sweep_superseded_plugin_siblings(
        &kimi_plugin_managed_dir(kimi_code_home),
        &[KIMI_PLUGIN_MANIFEST_RELATIVE],
    )
}

fn deferred_user_action_error(action: DeferredUserAction) -> TraceDecayError {
    TraceDecayError::Config {
        message: action.remediation,
    }
}

fn kimi_official_lifecycle_unavailable(
    action: &str,
    staged_dir: Option<&Path>,
) -> DeferredUserAction {
    let command = staged_dir.map_or_else(
        || format!("/plugins {action} {KIMI_PLUGIN_ID}"),
        |path| format!("/plugins {action} {}", path.display()),
    );
    DeferredUserAction {
        remediation: format!(
            "Kimi Code exposes plugin {action} only through the interactive `/plugins` host API; \
             TraceDecay made no current plugin registration changes. Open Kimi Code and run \
             `{command}`, then re-run repair to verify registration"
        ),
        staged_paths: staged_dir.into_iter().map(Path::to_path_buf).collect(),
    }
}

fn render_kimi_hook_commands(raw: &str, tracedecay_bin: &str) -> Result<String> {
    let mut manifest: serde_json::Value = serde_json::from_str(raw)?;
    let hooks = manifest
        .get_mut("hooks")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| TraceDecayError::Config {
            message: "Kimi plugin manifest is missing hooks".to_string(),
        })?;
    for hook in hooks {
        let Some(command) = hook.get_mut("command") else {
            continue;
        };
        match command.as_str() {
            Some("__TRACEDECAY_BIN__") => {
                *command = serde_json::Value::String(tracedecay_bin.to_string());
            }
            Some("__TRACEDECAY_SYNC__") => {
                *command = serde_json::Value::String(super::hook_command(
                    tracedecay_bin,
                    "hook-kimi-event",
                ));
            }
            Some("__TRACEDECAY_STOP__") => {
                *command = serde_json::Value::String(super::hook_command(
                    tracedecay_bin,
                    "hook-kimi-event",
                ));
            }
            _ => {}
        }
    }
    let rendered = format!("{}\n", serde_json::to_string_pretty(&manifest)?);
    if [
        "__TRACEDECAY_BIN__",
        "__TRACEDECAY_SYNC__",
        "__TRACEDECAY_STOP__",
    ]
    .iter()
    .any(|placeholder| rendered.contains(placeholder))
    {
        return Err(TraceDecayError::Config {
            message: "Kimi Hook V2 manifest retained an unresolved TraceDecay placeholder"
                .to_string(),
        });
    }
    Ok(rendered)
}

// ---------------------------------------------------------------------------
// Install helpers
// ---------------------------------------------------------------------------

/// Register tracedecay under `mcpServers` in a Kimi Code MCP config. Used by
/// the project-local install surface (`<project>/.kimi-code/mcp.json`).
fn install_mcp_server(mcp_path: &Path, tracedecay_bin: &str) -> Result<()> {
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

    settings["mcpServers"]["tracedecay"] = json!({
        "command": tracedecay_bin,
        "args": ["serve"]
    });

    safe_write_json_file(mcp_path, &settings, backup.as_deref())?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Added tracedecay MCP server to {}",
        mcp_path.display()
    );
    Ok(())
}

/// Install-or-refresh prompt rules in AGENTS.md.
fn install_prompt_rules(agents_md: &Path) -> Result<()> {
    let block = super::prompt_rules::standard_prompt_rules(
        PROMPT_RULE_MARKER,
        &PromptRulesOptions {
            extra_paragraphs: &[],
        },
    );
    super::prompt_rules::reconcile_prompt_rules(agents_md, PROMPT_RULE_MARKER, &block)
}

// ---------------------------------------------------------------------------
// Uninstall helpers
// ---------------------------------------------------------------------------

/// Remove tracedecay from a Kimi MCP config, backing up before rewriting and
/// deleting the file when nothing else remains. Used for the legacy
/// `~/.kimi/mcp.json` uninstall shim and the `<kimi-code-home>/mcp.json`
/// install-time migration.
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

/// Remove tracedecay rules from AGENTS.md.
fn uninstall_prompt_rules(agents_md: &Path) {
    super::prompt_rules::remove_prompt_rules(agents_md, PROMPT_RULE_MARKER);
}

// ---------------------------------------------------------------------------
// Healthcheck helpers
// ---------------------------------------------------------------------------

/// Check the Kimi Code CLI native plugin: registered in `installed.json` and
/// its deployed manifest parses. Like the other plugin-based hosts, an absent
/// plugin warns (not every machine runs Kimi Code CLI); a broken one fails.
fn doctor_check_plugin(dc: &mut DoctorCounters, kimi_code_home: &Path) {
    let installed_path = kimi_installed_json_path(kimi_code_home);
    if !installed_json_has_tracedecay(kimi_code_home) {
        dc.warn(&format!(
            "no tracedecay entry in {} — run `tracedecay install --agent kimi` if you use Kimi Code CLI",
            installed_path.display()
        ));
        return;
    }
    dc.pass(&format!(
        "Kimi Code CLI plugin registered in {}",
        installed_path.display()
    ));

    let manifest_path = kimi_plugin_managed_dir(kimi_code_home).join(KIMI_PLUGIN_MANIFEST_RELATIVE);
    let manifest = std::fs::read_to_string(&manifest_path)
        .ok()
        .and_then(|contents| serde_json::from_str::<serde_json::Value>(&contents).ok());
    if let Some(manifest) = manifest {
        dc.pass(&format!(
            "Kimi Code CLI plugin manifest parses at {}",
            manifest_path.display()
        ));
        let hooks = manifest
            .get("hooks")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|hook| hook.get("event").and_then(serde_json::Value::as_str))
            .collect::<std::collections::BTreeSet<_>>();
        if hooks.contains("PostToolUse") && hooks.contains("Stop") {
            dc.pass("Kimi native PostToolUse and Stop hooks registered");
        } else {
            dc.fail("Kimi plugin is missing PostToolUse or Stop hooks");
        }
    } else {
        dc.fail(&format!(
            "Kimi Code CLI plugin manifest missing or invalid at {} — run `tracedecay install --agent kimi`",
            manifest_path.display()
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn install_context(home: &Path) -> InstallContext {
        InstallContext {
            home: home.to_path_buf(),
            tracedecay_bin: "/bin/tracedecay".to_string(),
            tool_permissions: Vec::new(),
            project_root: None,
            dashboard: false,
        }
    }

    #[test]
    fn staging_sweeps_owned_superseded_managed_plugin_siblings_only() {
        let home = tempfile::tempdir().unwrap();
        let plugins = home.path().join(".kimi-code/plugins/managed");
        let retired = plugins.join("tracedecay.pre-v2-adopt");
        let foreign = plugins.join("tracedecay.personal");
        for dir in [&retired, &foreign] {
            std::fs::create_dir_all(dir.join(".kimi-plugin")).unwrap();
            std::fs::write(
                dir.join(".kimi-plugin/plugin.json"),
                serde_json::to_vec(&json!({ "name": "tracedecay" })).unwrap(),
            )
            .unwrap();
        }

        stage_kimi_install_action(&install_context(home.path())).expect("staging should succeed");

        assert!(
            !retired.exists(),
            "a manifest-owned superseded managed plugin must be swept"
        );
        assert!(
            foreign.exists(),
            "an owned-looking sibling without an explicitly retired suffix must be preserved"
        );
    }
}
