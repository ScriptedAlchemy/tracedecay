//! Kimi Code CLI agent integration.
//!
//! Kimi Code currently exposes plugin lifecycle only through its interactive
//! `/plugins` host API. `TraceDecay` stages its first-party bundle under its
//! own profile, while registration in `plugins/installed.json` remains owned by Kimi's
//! interactive host flow. Until Kimi ships a documented non-interactive
//! mutation API, global install/update/uninstall return an explicit
//! remediation instead of mutating the current registration. Project-local `--local`
//! installs write
//! `<project>/.kimi-code/mcp.json` plus prompt rules in `<project>/AGENTS.md`.
//!
//! Kimi Code owns the plugin registry; TraceDecay owns only its staged source.
//!
//! **Deferral re-verified 2026-08-08 under the CLI-first policy.** `kimi
//! --help` was probed directly: its command set is
//! `export, provider, acp, web, server, login, doctor, vis, migrate, upgrade`
//! — there is no `mcp` subcommand and no plugin subcommand of any kind. The
//! documented way to add, edit, or delete a server is the in-TUI
//! `/mcp-config`. So there is nothing to adopt, and the deferral above is the
//! honest lifecycle rather than a preference. See
//! <https://www.kimi.com/code/docs/en/kimi-code-cli/customization/mcp.html>.

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::errors::{Result, TraceDecayError};

use super::{
    AgentIntegration, DeferredUserAction, DoctorCounters, HealthcheckContext, InstallContext,
    JsonConfigDialect, McpUninstallPolicy, NonInteractiveInstallOutcome, UpdatePluginOutcome,
    host_home_override, install_mcp_server_entry, load_json_file, load_json_file_strict,
    mcp_config_has_tracedecay, safe_write_text_file, uninstall_mcp_server_entry,
};

use super::prompt_rules::{PROMPT_RULE_MARKER, PromptRulesOptions};

/// Environment variable that overrides the Kimi Code CLI home directory.
/// When unset, the home resolves to `~/.kimi-code`.
pub const KIMI_CODE_HOME_ENV: &str = "KIMI_CODE_HOME";

/// Plugin id read from Kimi Code CLI's official installed-plugin state.
const KIMI_PLUGIN_ID: &str = "tracedecay";

/// Deploy-relative path of the Kimi Code plugin manifest in its staged source.
const KIMI_PLUGIN_MANIFEST_RELATIVE: &str = ".kimi-plugin/plugin.json";

/// Profile-relative source directory passed to Kimi's native `/plugins` flow.
pub(crate) const KIMI_STAGED_PLUGIN_RELATIVE: &str =
    ".tracedecay/host-bundle-stage/kimi/tracedecay";

pub struct KimiIntegration;

impl AgentIntegration for KimiIntegration {
    fn name(&self) -> &'static str {
        "Kimi CLI"
    }

    fn id(&self) -> &'static str {
        "kimi"
    }

    fn preflight_non_interactive_install(
        &self,
        ctx: &InstallContext,
    ) -> Result<NonInteractiveInstallOutcome> {
        if kimi_plugin_is_natively_active(&ctx.home, &kimi_code_home(&ctx.home))? {
            return Ok(NonInteractiveInstallOutcome::Ready);
        }
        Ok(NonInteractiveInstallOutcome::DeferredUserAction(
            kimi_official_lifecycle_unavailable("install", None),
        ))
    }

    fn prepare_non_interactive_install(
        &self,
        ctx: &InstallContext,
    ) -> Result<NonInteractiveInstallOutcome> {
        let deferred = stage_kimi_install_action(ctx)?;
        if kimi_plugin_is_natively_active(&ctx.home, &kimi_code_home(&ctx.home))? {
            Ok(NonInteractiveInstallOutcome::Ready)
        } else {
            Ok(NonInteractiveInstallOutcome::DeferredUserAction(deferred))
        }
    }

    fn interactive_activation_guidance(&self) -> Option<String> {
        Some(kimi_official_lifecycle_unavailable("install", None).remediation)
    }

    fn interactive_removal_guidance(&self) -> Option<String> {
        Some(kimi_official_lifecycle_unavailable("remove", None).remediation)
    }

    fn supports_local_install(&self) -> bool {
        true
    }

    #[hotpath::measure(label = "hosts.agent.kimi.project_install")]
    fn activate_project_host_component_registration(
        &self,
        _components: &[super::host_bundle_v2::HostBundleComponentV1],
        ctx: &InstallContext,
        project_path: &Path,
    ) -> Result<()> {
        let mcp_path = project_path.join(".kimi-code/mcp.json");
        let agents_md = project_path.join("AGENTS.md");
        super::ensure_project_local_safe_paths(
            project_path,
            [mcp_path.as_path(), agents_md.as_path()],
        )?;
        std::fs::create_dir_all(project_path.join(".kimi-code"))?;
        install_mcp_server_entry(
            &mcp_path,
            "mcpServers",
            json!({
                "command": ctx.tracedecay_bin.clone(),
                "args": ["serve"]
            }),
            "Kimi",
            JsonConfigDialect::Json,
        )?;
        install_prompt_rules(&agents_md)?;
        super::install_managed_skill_prompt_index(
            &ctx.home,
            &agents_md,
            tracedecay_automation_runtime::automation::skill_targets::SkillInstallTarget::Kimi,
        )
    }

    fn project_host_component_registration_paths(
        &self,
        _components: &[super::host_bundle_v2::HostBundleComponentV1],
        _home: &Path,
        project_path: &Path,
    ) -> Result<Vec<PathBuf>> {
        Ok(vec![
            project_path.join(".kimi-code/mcp.json"),
            project_path.join("AGENTS.md"),
        ])
    }

    fn deactivate_project_host_component_registration(
        &self,
        _components: &[super::host_bundle_v2::HostBundleComponentV1],
        ctx: &InstallContext,
        project_path: &Path,
    ) -> Result<()> {
        let mcp_path = project_path.join(".kimi-code/mcp.json");
        uninstall_mcp_server_entry(
            &mcp_path,
            "mcpServers",
            JsonConfigDialect::Json,
            McpUninstallPolicy {
                prune_empty_root: true,
                remove_empty_file: true,
            },
        )?;
        let agents_md = project_path.join("AGENTS.md");
        super::remove_managed_skill_prompt_index(
            &ctx.home,
            &agents_md,
            tracedecay_automation_runtime::automation::skill_targets::SkillInstallTarget::Kimi,
        )?;
        uninstall_prompt_rules(&agents_md)?;
        Ok(())
    }

    fn update_plugin(&self, ctx: &InstallContext) -> Result<UpdatePluginOutcome> {
        let code_home = kimi_code_home(&ctx.home);
        if !installed_json_has_tracedecay(&code_home) {
            return Ok(UpdatePluginOutcome::NotInstalled);
        }
        stage_kimi_install_action(ctx).map(UpdatePluginOutcome::DeferredUserAction)
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mKimi CLI integration\x1b[0m");
        doctor_check_plugin(dc, &ctx.home, &kimi_code_home(&ctx.home));
    }

    fn reports_absence_to_doctor(&self) -> bool {
        true
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
        if !kimi_manager_points_at_staged_source(entry, &ctx.home) {
            return State::Repairable;
        }
        let staged_dir = kimi_staged_plugin_dir(&ctx.home);
        let manifest_path = staged_dir.join(KIMI_PLUGIN_MANIFEST_RELATIVE);
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
        if kimi_plugin_is_natively_active(&ctx.home, &code_home)? {
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
        installed_json_has_tracedecay(&kimi_code_home(home))
    }

    fn export_managed_skills_local(
        &self,
        project_root: &Path,
        profile_root: &Path,
    ) -> Result<Vec<tracedecay_automation_runtime::automation::skill_targets::SkillInstallSummary>>
    {
        let agents_md = project_root.join("AGENTS.md");
        if !mcp_config_has_tracedecay(
            &project_root.join(".kimi-code/mcp.json"),
            "mcpServers",
            load_json_file,
        ) || !agents_md.exists()
        {
            return Ok(Vec::new());
        }
        Ok(vec![
            tracedecay_automation_runtime::automation::skill_targets::install_managed_skills(
                &crate::host_io(),
                profile_root,
                tracedecay_automation_runtime::automation::skill_targets::SkillInstallTarget::Kimi,
                &agents_md,
            )?,
        ])
    }
}

// ---------------------------------------------------------------------------
// Kimi Code CLI native plugin helpers
// ---------------------------------------------------------------------------

/// Resolve the Kimi Code CLI home: `$KIMI_CODE_HOME` when set, non-empty, and
/// under the admitted `home`; otherwise `~/.kimi-code`.
fn kimi_code_home(home: &Path) -> PathBuf {
    host_home_override(home, KIMI_CODE_HOME_ENV, ".kimi-code")
}

/// The staged source Kimi's native plugin command consumes.
pub(crate) fn kimi_staged_plugin_dir(home: &Path) -> PathBuf {
    home.join(KIMI_STAGED_PLUGIN_RELATIVE)
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

fn kimi_plugin_is_natively_active(home: &Path, code_home: &Path) -> Result<bool> {
    let installed_path = kimi_installed_json_path(code_home);
    if !installed_path.exists() {
        return Ok(false);
    }
    let installed =
        load_json_file_strict(&installed_path).map_err(|error| TraceDecayError::Config {
            message: format!(
                "could not read Kimi native plugin registration at {}: {error}",
                installed_path.display()
            ),
        })?;
    Ok(kimi_installed_entry(&installed)
        .is_some_and(|entry| kimi_manager_points_at_staged_source(entry, home)))
}

/// True when Kimi's `installed.json` entry is enabled, sourced from a local
/// path, and that path is the TraceDecay-staged plugin directory.
fn kimi_manager_points_at_staged_source(entry: &serde_json::Value, home: &Path) -> bool {
    let staged_dir = kimi_staged_plugin_dir(home);
    let expected_root = staged_dir
        .canonicalize()
        .unwrap_or_else(|_| staged_dir.clone());
    entry.get("enabled").and_then(serde_json::Value::as_bool) != Some(false)
        && entry.get("source").and_then(serde_json::Value::as_str) == Some("local-path")
        && entry
            .get("root")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|root| {
                let root = Path::new(root);
                root.canonicalize().unwrap_or_else(|_| root.to_path_buf()) == expected_root
            })
}

/// Canonical rendered Kimi Code plugin inventory shared by native-activation
/// staging and the receipt-backed first-party catalog.
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

#[hotpath::measure(label = "hosts.agent.kimi.plugin_deploy")]
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
    let staged_dir = kimi_staged_plugin_dir(&ctx.home);
    deploy_kimi_plugin_to(&staged_dir, &ctx.tracedecay_bin)?;
    Ok(kimi_official_lifecycle_unavailable(
        "install",
        Some(&staged_dir),
    ))
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
            Some(super::plugin_bundle::TRACEDECAY_BIN_PLACEHOLDER) => {
                *command = serde_json::Value::String(tracedecay_bin.to_string());
            }
            Some(
                super::plugin_bundle::TRACEDECAY_SYNC_PLACEHOLDER
                | super::plugin_bundle::TRACEDECAY_STOP_PLACEHOLDER,
            ) => {
                *command = serde_json::Value::String(super::hook_command(
                    tracedecay_bin,
                    "hook-kimi-event",
                ));
            }
            _ => {}
        }
    }
    let rendered = format!("{}\n", serde_json::to_string_pretty(&manifest)?);
    super::plugin_bundle::reject_unresolved_placeholders(&rendered, "Kimi Hook V2 manifest")?;
    Ok(rendered)
}

// ---------------------------------------------------------------------------
// Install helpers
// ---------------------------------------------------------------------------

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

/// Remove tracedecay rules from AGENTS.md.
fn uninstall_prompt_rules(agents_md: &Path) -> Result<()> {
    super::prompt_rules::remove_standard_prompt_rules(agents_md)
}

// ---------------------------------------------------------------------------
// Healthcheck helpers
// ---------------------------------------------------------------------------

/// Check the Kimi Code CLI native plugin: registered in `installed.json` and
/// its deployed manifest parses. Like the other plugin-based hosts, an absent
/// plugin warns (not every machine runs Kimi Code CLI); a broken one fails.
fn doctor_check_plugin(dc: &mut DoctorCounters, home: &Path, kimi_code_home: &Path) {
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

    let manifest_path = kimi_staged_plugin_dir(home).join(KIMI_PLUGIN_MANIFEST_RELATIVE);
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

    fn installed_prompt(path: &Path, operator_contents: Option<&[u8]>) {
        if let Some(contents) = operator_contents {
            std::fs::write(path, contents).unwrap();
        }
        install_prompt_rules(path).unwrap();
    }

    fn start_paused_uninstall(
        path: &Path,
    ) -> (
        crate::agents::TestHostConfigWritePauseController,
        std::thread::JoinHandle<std::result::Result<(), String>>,
    ) {
        let pause = crate::agents::pause_next_host_config_write_at_publication(path);
        let writer_path = path.to_path_buf();
        let remover = std::thread::spawn(move || {
            uninstall_prompt_rules(&writer_path).map_err(|error| error.to_string())
        });
        pause.wait_until_reached();
        (pause, remover)
    }

    #[test]
    fn kimi_prompt_uninstall_refuses_a_concurrent_nonempty_rewrite() {
        let root = tempfile::tempdir().unwrap();
        let prompt = root.path().join("AGENTS.md");
        installed_prompt(&prompt, Some(b"operator rules\n"));
        let (pause, remover) = start_paused_uninstall(&prompt);

        let foreign = b"foreign Kimi edit\n";
        std::fs::write(&prompt, foreign).unwrap();
        pause.resume();
        let error = remover.join().unwrap().unwrap_err();

        assert!(error.contains("changed since it was read"), "{error}");
        assert_eq!(std::fs::read(&prompt).unwrap(), foreign);
    }

    #[test]
    fn kimi_prompt_uninstall_refuses_a_concurrent_empty_deletion() {
        let root = tempfile::tempdir().unwrap();
        let prompt = root.path().join("AGENTS.md");
        installed_prompt(&prompt, None);
        let (pause, remover) = start_paused_uninstall(&prompt);

        let foreign = b"foreign Kimi edit\n";
        std::fs::write(&prompt, foreign).unwrap();
        pause.resume();
        let error = remover.join().unwrap().unwrap_err();

        assert!(error.contains("changed since it was read"), "{error}");
        assert_eq!(std::fs::read(&prompt).unwrap(), foreign);
    }

    #[test]
    fn kimi_prompt_uninstall_rewrites_operator_content_and_deletes_an_empty_result() {
        let root = tempfile::tempdir().unwrap();
        let nonempty = root.path().join("nonempty.md");
        installed_prompt(&nonempty, Some(b"operator rules\n"));

        uninstall_prompt_rules(&nonempty).unwrap();

        assert_eq!(std::fs::read(&nonempty).unwrap(), b"operator rules\n");

        let empty = root.path().join("empty.md");
        installed_prompt(&empty, None);

        uninstall_prompt_rules(&empty).unwrap();

        assert!(!empty.exists());
    }

    #[cfg(unix)]
    #[test]
    fn kimi_prompt_uninstall_refuses_a_symlink_swap() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let prompt = root.path().join("AGENTS.md");
        let outside = root.path().join("outside.md");
        installed_prompt(&prompt, None);
        std::fs::write(&outside, b"outside Kimi rules\n").unwrap();
        let (pause, remover) = start_paused_uninstall(&prompt);

        std::fs::remove_file(&prompt).unwrap();
        symlink(&outside, &prompt).unwrap();
        pause.resume();
        let error = remover.join().unwrap().unwrap_err();

        assert!(error.contains("unsafe host metadata path"), "{error}");
        assert!(
            std::fs::symlink_metadata(&prompt)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(&outside).unwrap(), b"outside Kimi rules\n");
    }

    #[cfg(unix)]
    #[test]
    fn kimi_prompt_uninstall_refuses_a_metadata_change() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let root = tempfile::tempdir().unwrap();
        let prompt = root.path().join("AGENTS.md");
        installed_prompt(&prompt, Some(b"operator rules\n"));
        let before = std::fs::read(&prompt).unwrap();
        std::fs::set_permissions(&prompt, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (pause, remover) = start_paused_uninstall(&prompt);

        std::fs::set_permissions(&prompt, std::fs::Permissions::from_mode(0o640)).unwrap();
        pause.resume();
        let error = remover.join().unwrap().unwrap_err();

        assert!(error.contains("changed since it was read"), "{error}");
        assert_eq!(std::fs::read(&prompt).unwrap(), before);
        assert_eq!(std::fs::metadata(&prompt).unwrap().mode() & 0o777, 0o640);
    }

    #[test]
    fn kimi_prompt_uninstall_refuses_a_missing_file_race() {
        let root = tempfile::tempdir().unwrap();
        let prompt = root.path().join("AGENTS.md");
        installed_prompt(&prompt, None);
        let (pause, remover) = start_paused_uninstall(&prompt);

        std::fs::remove_file(&prompt).unwrap();
        pause.resume();
        let error = remover.join().unwrap().unwrap_err();

        assert!(error.contains("failed to conditionally remove"), "{error}");
        assert!(!prompt.exists());
    }
}
