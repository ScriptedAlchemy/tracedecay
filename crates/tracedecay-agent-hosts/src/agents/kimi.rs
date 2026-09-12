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

use tracedecay_domain::errors::{Result, TraceDecayError};

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
        if kimi_plugin_is_natively_active(
            &ctx.home,
            &kimi_code_home(&ctx.home),
            &ctx.tracedecay_bin,
        )? {
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
        if kimi_plugin_is_natively_active(
            &ctx.home,
            &kimi_code_home(&ctx.home),
            &ctx.tracedecay_bin,
        )? {
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
        _components: &[super::host_bundle::HostBundleComponentV1],
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
        _components: &[super::host_bundle::HostBundleComponentV1],
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
        _components: &[super::host_bundle::HostBundleComponentV1],
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
        component: super::host_bundle::HostBundleComponentV1,
        ctx: &HealthcheckContext,
    ) -> super::host_bundle::HostBundleRegistrationStateV1 {
        use super::host_bundle::{HostBundleComponentV1, HostBundleRegistrationStateV1 as State};

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
        if !kimi_manager_has_active_staged_install(entry, &ctx.home, &code_home) {
            return State::Repairable;
        }
        match kimi_managed_bundle_matches_staged(&ctx.home, &code_home) {
            Ok(true) => {}
            Ok(false) => return State::Repairable,
            Err(_) => return State::Corrupt,
        }
        let manifest_path = kimi_managed_plugin_dir(&code_home).join(KIMI_PLUGIN_MANIFEST_RELATIVE);
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
        if kimi_plugin_is_natively_active(&ctx.home, &code_home, &ctx.tracedecay_bin)? {
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

fn kimi_managed_plugin_dir(kimi_code_home: &Path) -> PathBuf {
    kimi_code_home.join("plugins/managed/tracedecay")
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

fn kimi_plugin_is_natively_active(
    home: &Path,
    code_home: &Path,
    tracedecay_bin: &str,
) -> Result<bool> {
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
    let Some(entry) = kimi_installed_entry(&installed) else {
        return Ok(false);
    };
    if !kimi_manager_has_active_staged_install(entry, home, code_home) {
        return Ok(false);
    }
    kimi_managed_bundle_matches_rendered(code_home, tracedecay_bin)
}

/// True when Kimi's native manager has enabled its managed copy of the
/// TraceDecay-staged local plugin source.
fn kimi_manager_has_active_staged_install(
    entry: &serde_json::Value,
    home: &Path,
    code_home: &Path,
) -> bool {
    entry.get("enabled").and_then(serde_json::Value::as_bool) == Some(true)
        && entry.get("source").and_then(serde_json::Value::as_str) == Some("local-path")
        && kimi_manager_path_matches(entry, "root", &kimi_managed_plugin_dir(code_home))
        && kimi_manager_path_matches(entry, "originalSource", &kimi_staged_plugin_dir(home))
}

fn kimi_managed_bundle_matches_rendered(code_home: &Path, tracedecay_bin: &str) -> Result<bool> {
    let rendered = rendered_plugin_files(tracedecay_bin)?;
    let (expected, relatives) = super::rendered_bundle_content_digest(&rendered)?;
    Ok(
        super::observed_bundle_content_digest(&kimi_managed_plugin_dir(code_home), &relatives)?
            == Some(expected),
    )
}

fn kimi_managed_bundle_matches_staged(home: &Path, code_home: &Path) -> Result<bool> {
    let rendered = rendered_plugin_files("tracedecay")?;
    let (_, relatives) = super::rendered_bundle_content_digest(&rendered)?;
    let Some(staged) =
        super::observed_bundle_content_digest(&kimi_staged_plugin_dir(home), &relatives)?
    else {
        return Ok(false);
    };
    Ok(
        super::observed_bundle_content_digest(&kimi_managed_plugin_dir(code_home), &relatives)?
            == Some(staged),
    )
}

fn kimi_manager_path_matches(entry: &serde_json::Value, field: &str, expected: &Path) -> bool {
    let Some(path) = entry.get(field).and_then(serde_json::Value::as_str) else {
        return false;
    };
    let Ok(expected) = expected.canonicalize() else {
        return false;
    };
    Path::new(path)
        .canonicalize()
        .is_ok_and(|path| path == expected)
}

/// Canonical rendered Kimi Code plugin inventory shared by native-activation
/// staging and the receipt-backed first-party catalog.
pub(crate) fn rendered_plugin_files(tracedecay_bin: &str) -> Result<Vec<(&'static str, String)>> {
    super::plugin_bundle::kimi_files()
        .into_iter()
        .map(|(relative, contents)| {
            let rendered = if relative == KIMI_PLUGIN_MANIFEST_RELATIVE {
                let stamped = super::plugin_bundle::stamp_manifest_version(contents)?;
                // Kimi resolves plugin MCP executables from PATH and rejects
                // absolute commands. Keep the template's `tracedecay` command;
                // hooks are shell commands and may use the resolved path.
                render_kimi_hook_commands(&stamped, tracedecay_bin)?
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
/// its host-managed bundle matches the staged source. Like the other
/// plugin-based hosts, an absent plugin warns (not every machine runs Kimi
/// Code CLI); a broken one fails.
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

    match kimi_managed_bundle_matches_staged(home, kimi_code_home) {
        Ok(true) => dc.pass("Kimi Code CLI managed plugin matches its staged source"),
        Ok(false) => dc.fail(
            "Kimi Code CLI managed plugin is stale — run the staged `/plugins install` action",
        ),
        Err(error) => dc.fail(&format!(
            "could not verify Kimi Code CLI managed plugin: {error}"
        )),
    }

    let manifest_path = kimi_managed_plugin_dir(kimi_code_home).join(KIMI_PLUGIN_MANIFEST_RELATIVE);
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

    #[test]
    fn native_activation_waits_for_manager_to_copy_refreshed_staged_bundle() {
        let home = tempfile::tempdir().unwrap();
        let code_home = home.path().join(".kimi-code");
        let staged_source = kimi_staged_plugin_dir(home.path());
        let managed_root = code_home.join("plugins/managed/tracedecay");
        deploy_kimi_plugin_to(&staged_source, "/old/tracedecay").unwrap();
        deploy_kimi_plugin_to(&managed_root, "/old/tracedecay").unwrap();
        std::fs::create_dir_all(code_home.join("plugins")).unwrap();
        std::fs::write(
            kimi_installed_json_path(&code_home),
            serde_json::to_vec(&json!({
                "version": 1,
                "plugins": [{
                    "id": "tracedecay",
                    "root": managed_root,
                    "source": "local-path",
                    "originalSource": staged_source,
                    "enabled": true,
                    "installedAt": "2026-09-12T00:00:00Z",
                    "updatedAt": "2026-09-12T00:00:00.000Z"
                }]
            }))
            .unwrap(),
        )
        .unwrap();
        let ctx = InstallContext {
            home: home.path().to_path_buf(),
            tracedecay_bin: "/new/tracedecay".to_string(),
            tool_permissions: Vec::new(),
            project_root: None,
            dashboard: false,
        };

        assert!(matches!(
            KimiIntegration
                .prepare_non_interactive_install(&ctx)
                .unwrap(),
            NonInteractiveInstallOutcome::DeferredUserAction(_)
        ));
        let health_ctx = HealthcheckContext {
            home: home.path().to_path_buf(),
            project_path: home.path().join("project"),
        };
        assert_eq!(
            KimiIntegration.host_component_registration(
                super::super::host_bundle::HostBundleComponentV1::Core,
                &health_ctx,
            ),
            super::super::host_bundle::HostBundleRegistrationStateV1::Repairable
        );

        deploy_kimi_plugin_to(&managed_root, &ctx.tracedecay_bin).unwrap();
        assert_eq!(
            KimiIntegration
                .prepare_non_interactive_install(&ctx)
                .unwrap(),
            NonInteractiveInstallOutcome::Ready
        );
        assert_eq!(
            KimiIntegration.host_component_registration(
                super::super::host_bundle::HostBundleComponentV1::Core,
                &health_ctx,
            ),
            super::super::host_bundle::HostBundleRegistrationStateV1::Current
        );
    }

    #[test]
    fn native_manager_recognizes_only_its_managed_copy_of_staged_source() {
        let home = tempfile::tempdir().unwrap();
        let code_home = home.path().join(".kimi-code");
        let staged_source = kimi_staged_plugin_dir(home.path());
        let managed_root = code_home.join("plugins/managed/tracedecay");
        std::fs::create_dir_all(&staged_source).unwrap();
        std::fs::create_dir_all(&managed_root).unwrap();

        // Sanitized from Kimi Code 0.42's host-owned installed.json after
        // `/plugins install <TraceDecay staged source>`.
        let installed = json!({
            "id": "tracedecay",
            "root": managed_root,
            "source": "local-path",
            "originalSource": staged_source,
            "enabled": true,
            "installedAt": "2026-09-12T00:00:00Z",
            "updatedAt": "2026-09-12T00:00:00.000Z"
        });
        assert!(kimi_manager_has_active_staged_install(
            &installed,
            home.path(),
            &code_home
        ));

        let mut missing_source = installed.clone();
        missing_source
            .as_object_mut()
            .unwrap()
            .remove("originalSource");
        assert!(!kimi_manager_has_active_staged_install(
            &missing_source,
            home.path(),
            &code_home
        ));

        let mut foreign_source = installed.clone();
        foreign_source["originalSource"] = json!(home.path().join("foreign-plugin"));
        assert!(!kimi_manager_has_active_staged_install(
            &foreign_source,
            home.path(),
            &code_home
        ));

        let mut foreign_root = installed;
        foreign_root["root"] = json!(code_home.join("plugins/managed/foreign-plugin"));
        assert!(!kimi_manager_has_active_staged_install(
            &foreign_root,
            home.path(),
            &code_home
        ));
    }

    #[test]
    fn rendered_plugin_uses_kimi_supported_mcp_command() {
        let manifest = rendered_plugin_files("/opt/tracedecay/bin/tracedecay")
            .unwrap()
            .into_iter()
            .find(|(relative, _)| *relative == KIMI_PLUGIN_MANIFEST_RELATIVE)
            .map(|(_, contents)| serde_json::from_str::<serde_json::Value>(&contents).unwrap())
            .unwrap();

        assert_eq!(
            manifest["mcpServers"]["tracedecay"]["command"],
            "tracedecay"
        );
        assert!(
            manifest["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .contains("/opt/tracedecay/bin/tracedecay")
        );
    }
}
