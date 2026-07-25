// Rust guideline compliant 2025-10-17
//! Kimi Code CLI agent integration.
//!
//! The global install is the Kimi Code CLI native plugin: the tracedecay
//! plugin bundle deploys to `<kimi-code-home>/plugins/managed/tracedecay/`
//! and registers in `<kimi-code-home>/plugins/installed.json`, where
//! `<kimi-code-home>` is `$KIMI_CODE_HOME` when set, else `~/.kimi-code`. The
//! plugin manifest owns the MCP server, skills, and commands, so a stale
//! direct registration in `<kimi-code-home>/mcp.json` is migrated away on
//! install. Project-local `--local` installs write
//! `<project>/.kimi-code/mcp.json` plus prompt rules in `<project>/AGENTS.md`.
//!
//! The pre-plugin Kimi CLI surface (`~/.kimi/mcp.json` + `~/.kimi/AGENTS.md`)
//! is no longer written; `uninstall` and `has_tracedecay` keep reading it as
//! a migration shim so installs written by older tracedecay versions are
//! still cleaned up and noticed by upgrade tracking.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::json;

use crate::errors::{Result, TraceDecayError};

use super::{
    AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext, UpdatePluginOutcome,
    backup_and_write_json, backup_config_file, load_json_file, load_json_file_strict,
    safe_write_json_file, safe_write_text_file,
};

use super::prompt_rules::{PROMPT_RULE_MARKER, PromptRulesOptions};

/// Environment variable that overrides the Kimi Code CLI home directory.
/// When unset, the home resolves to `~/.kimi-code`.
pub const KIMI_CODE_HOME_ENV: &str = "KIMI_CODE_HOME";

/// Plugin id the tracedecay bundle registers under in Kimi Code CLI's
/// `plugins/installed.json` and the name of its managed deploy directory.
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
        let code_home = kimi_code_home(&ctx.home);
        let staged_dir = ctx
            .home
            .join(".tracedecay/host-bundle-stage/kimi/tracedecay");
        deploy_kimi_plugin_to(&staged_dir, &ctx.tracedecay_bin)?;
        install_kimi_plugin_with_manager_fallback(&code_home, &staged_dir, &ctx.tracedecay_bin)?;
        let managed_dir = kimi_plugin_managed_dir(&code_home);
        migrate_kimi_code_mcp_json(&code_home);

        eprintln!();
        eprintln!("Setup complete. Next steps:");
        eprintln!("  1. cd into your project and run: tracedecay init");
        eprintln!(
            "  2. Start a new Kimi Code session — the tracedecay plugin loads from {}",
            managed_dir.display()
        );
        Ok(())
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
        // The managed plugin dir is a tracedecay-generated bundle (its
        // manifest is a rendered artifact, not user config), so refreshing it
        // is exactly the install path. `plugins/installed.json` is a
        // tracedecay-owned registry entry: the refresh bumps its `updatedAt`
        // while preserving the user's `enabled`/`installedAt` values.
        let code_home = kimi_code_home(&ctx.home);
        if !installed_json_has_tracedecay(&code_home) {
            return Ok(UpdatePluginOutcome::NotInstalled);
        }
        let staged_dir = ctx
            .home
            .join(".tracedecay/host-bundle-stage/kimi/tracedecay");
        deploy_kimi_plugin_to(&staged_dir, &ctx.tracedecay_bin)?;
        install_kimi_plugin_with_manager_fallback(&code_home, &staged_dir, &ctx.tracedecay_bin)?;
        let managed_dir = kimi_plugin_managed_dir(&code_home);
        Ok(UpdatePluginOutcome::Refreshed(vec![managed_dir]))
    }

    fn uninstall(&self, ctx: &InstallContext) -> Result<()> {
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

        let code_home = kimi_code_home(&ctx.home);
        if let Err(error) = run_kimi_plugin_manager(["plugin", "remove", KIMI_PLUGIN_ID]) {
            eprintln!(
                "  Official Kimi plugin manager unavailable ({error}); \
                 removing the managed plugin registration directly."
            );
            remove_kimi_installed_entry(&code_home);
        }
        remove_kimi_plugin_dir(&code_home)?;

        eprintln!();
        eprintln!("Uninstall complete. Tracedecay has been removed from Kimi Code CLI.");
        eprintln!("Start a new Kimi Code session for changes to take effect.");
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
        // Same contract as `install_kimi_plugin_with_manager_fallback`:
        // activation must not hard-depend on the optional external `kimi`
        // binary. Without this fallback, a kimi CLI lacking the `plugin`
        // subcommand (exit 1: "unknown command 'plugin'") failed the v2
        // lifecycle between apply and commit on every run, leaving the shared
        // component-set journal wedged — which then blocked EVERY host's
        // bundle lifecycle until the journal was manually cleared, and the
        // receiptless artifacts re-failed the next run as ownership conflicts.
        let code_home = kimi_code_home(&ctx.home);
        let managed_dir = kimi_plugin_managed_dir(&code_home);
        match run_kimi_plugin_manager(["plugin", "install", managed_dir.to_string_lossy().as_ref()])
        {
            Ok(()) => Ok(()),
            Err(error) => {
                eprintln!(
                    "  Official Kimi plugin manager unavailable ({error}); \
                     registering the managed plugin directly."
                );
                // The v2 artifact writer has already deployed the managed dir;
                // only the registry entry is missing.
                upsert_kimi_installed_entry(&code_home)
            }
        }
    }

    fn deactivate_deployed_host_registration(&self, ctx: &InstallContext) -> Result<()> {
        let code_home = kimi_code_home(&ctx.home);
        remove_kimi_installed_entry(&code_home);
        prune_empty_kimi_plugin_dirs(&code_home, &ctx.tracedecay_bin)?;
        Ok(())
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

/// Deploy the embedded plugin bundle into the managed plugin dir, rendering
/// the manifest with the crate version and the resolved tracedecay binary.
/// Every file is written atomically; existing bundle files are overwritten.
fn deploy_kimi_plugin(kimi_code_home: &Path, tracedecay_bin: &str) -> Result<PathBuf> {
    let managed_dir = kimi_plugin_managed_dir(kimi_code_home);
    deploy_kimi_plugin_to(&managed_dir, tracedecay_bin)
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

/// Install the staged plugin through the official Kimi plugin manager when it
/// is available; otherwise fall back to the direct managed-dir deploy and
/// registry upsert the pre-manager installer used. Installs must not
/// hard-depend on the optional external `kimi` binary — offline machines and
/// CI environments without it still get a working managed plugin.
fn install_kimi_plugin_with_manager_fallback(
    code_home: &Path,
    staged_dir: &Path,
    tracedecay_bin: &str,
) -> Result<()> {
    match run_kimi_plugin_manager(["plugin", "install", staged_dir.to_string_lossy().as_ref()]) {
        Ok(()) => Ok(()),
        Err(error) => {
            eprintln!(
                "  Official Kimi plugin manager unavailable ({error}); \
                 deploying the managed plugin directly."
            );
            deploy_kimi_plugin(code_home, tracedecay_bin)?;
            upsert_kimi_installed_entry(code_home)
        }
    }
}

fn run_kimi_plugin_manager<'a>(args: impl IntoIterator<Item = &'a str>) -> Result<()> {
    let status =
        Command::new("kimi")
            .args(args)
            .status()
            .map_err(|error| TraceDecayError::Config {
                message: format!("failed to run official Kimi plugin manager: {error}"),
            })?;
    if status.success() {
        Ok(())
    } else {
        Err(TraceDecayError::Config {
            message: format!("official Kimi plugin manager exited with {status}"),
        })
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

/// Upsert the tracedecay entry in `<kimi-code-home>/plugins/installed.json`,
/// creating the registry (`{"version":1,"plugins":[]}`) when missing. An
/// existing entry keeps its `enabled` and `installedAt` values; `updatedAt`
/// always moves to now. Written atomically with a `.bak` backup.
fn upsert_kimi_installed_entry(kimi_code_home: &Path) -> Result<()> {
    let installed_path = kimi_installed_json_path(kimi_code_home);
    let backup = backup_config_file(&installed_path)?;
    let mut installed = if installed_path.exists() {
        let value = load_json_file_strict(&installed_path)?;
        if value.is_object() {
            value
        } else {
            return Err(TraceDecayError::Config {
                message: format!(
                    "{} is not a JSON object; fix or delete it manually",
                    installed_path.display()
                ),
            });
        }
    } else {
        json!({"version": 1, "plugins": []})
    };
    if installed.get("version").is_none() {
        installed["version"] = json!(1);
    }
    if installed.get("plugins").is_none() {
        installed["plugins"] = json!([]);
    }
    let Some(plugins) = installed
        .get_mut("plugins")
        .and_then(|value| value.as_array_mut())
    else {
        return Err(TraceDecayError::Config {
            message: format!(
                "{} has a non-array \"plugins\" field; fix or delete it manually",
                installed_path.display()
            ),
        });
    };

    let now = crate::timeutil::now_iso_utc();
    let managed_dir = kimi_plugin_managed_dir(kimi_code_home);
    let root = managed_dir
        .canonicalize()
        .unwrap_or_else(|_| managed_dir.clone());
    let existing = plugins
        .iter()
        .position(|entry| entry.get("id").and_then(|value| value.as_str()) == Some(KIMI_PLUGIN_ID));
    let (enabled, installed_at) = existing.map(|index| &plugins[index]).map_or_else(
        || (true, now.clone()),
        |entry| {
            (
                entry
                    .get("enabled")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true),
                entry
                    .get("installedAt")
                    .and_then(|value| value.as_str())
                    .map_or_else(|| now.clone(), str::to_string),
            )
        },
    );
    let entry = json!({
        "id": KIMI_PLUGIN_ID,
        "root": root,
        "source": "local-path",
        "enabled": enabled,
        "installedAt": installed_at,
        "updatedAt": now,
    });
    match existing {
        Some(index) => plugins[index] = entry,
        None => plugins.push(entry),
    }

    safe_write_json_file(&installed_path, &installed, backup.as_deref())?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Registered Kimi Code CLI plugin in {}",
        installed_path.display()
    );
    Ok(())
}

/// Remove the tracedecay entry from `installed.json`, leaving the file (and
/// any other entries) in place with an empty `plugins` array when tracedecay
/// was the only one. Best-effort, mirroring the other uninstall helpers.
fn remove_kimi_installed_entry(kimi_code_home: &Path) {
    let installed_path = kimi_installed_json_path(kimi_code_home);
    if !installed_path.exists() {
        eprintln!("  {} not found, skipping", installed_path.display());
        return;
    }
    let Ok(mut installed) = load_json_file_strict(&installed_path) else {
        return;
    };
    let Some(plugins) = installed
        .get_mut("plugins")
        .and_then(|value| value.as_array_mut())
    else {
        eprintln!(
            "  No tracedecay plugin entry in {}, skipping",
            installed_path.display()
        );
        return;
    };
    let before = plugins.len();
    plugins
        .retain(|entry| entry.get("id").and_then(|value| value.as_str()) != Some(KIMI_PLUGIN_ID));
    if plugins.len() == before {
        eprintln!(
            "  No tracedecay plugin entry in {}, skipping",
            installed_path.display()
        );
        return;
    }
    if backup_and_write_json(&installed_path, &installed) {
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed tracedecay plugin entry from {}",
            installed_path.display()
        );
    }
}

/// Delete the managed plugin dir recursively. A symlink or file at that path
/// is unlinked instead of followed.
fn remove_kimi_plugin_dir(kimi_code_home: &Path) -> Result<()> {
    let managed_dir = kimi_plugin_managed_dir(kimi_code_home);
    let Ok(metadata) = std::fs::symlink_metadata(&managed_dir) else {
        eprintln!("  {} not found, skipping", managed_dir.display());
        return Ok(());
    };
    if metadata.file_type().is_symlink() || metadata.is_file() {
        std::fs::remove_file(&managed_dir).map_err(|e| TraceDecayError::Config {
            message: format!("failed to remove {}: {e}", managed_dir.display()),
        })?;
    } else {
        std::fs::remove_dir_all(&managed_dir).map_err(|e| TraceDecayError::Config {
            message: format!("failed to remove {}: {e}", managed_dir.display()),
        })?;
    }
    eprintln!(
        "\x1b[32m✔\x1b[0m Removed Kimi Code CLI plugin at {}",
        managed_dir.display()
    );
    Ok(())
}

/// Remove only empty directories from the receipt-backed managed inventory.
/// Artifact files are owned and removed by the component transaction; this
/// cleanup never recursively deletes an unexpected file.
fn prune_empty_kimi_plugin_dirs(kimi_code_home: &Path, tracedecay_bin: &str) -> Result<()> {
    let managed_dir = kimi_plugin_managed_dir(kimi_code_home);
    let mut directories = std::collections::BTreeSet::new();
    directories.insert(managed_dir.clone());
    for (relative, _) in rendered_plugin_files(tracedecay_bin)? {
        let mut parent = Path::new(relative).parent();
        while let Some(relative_dir) = parent.filter(|path| !path.as_os_str().is_empty()) {
            directories.insert(managed_dir.join(relative_dir));
            parent = relative_dir.parent();
        }
    }
    let mut directories = directories.into_iter().collect::<Vec<_>>();
    directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for directory in directories {
        match std::fs::remove_dir(&directory) {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                ) => {}
            Err(error) => {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "failed to prune managed Kimi plugin directory {}: {error}",
                        directory.display()
                    ),
                });
            }
        }
    }
    Ok(())
}

/// Migration: the plugin manifest now provides the tracedecay MCP server, so
/// drop a stale direct registration from `<kimi-code-home>/mcp.json` (backup
/// first; the file is deleted when nothing else remains).
fn migrate_kimi_code_mcp_json(kimi_code_home: &Path) {
    let mcp_path = kimi_code_home.join("mcp.json");
    let has_tracedecay = mcp_path.exists()
        && load_json_file(&mcp_path)
            .get("mcpServers")
            .and_then(|servers| servers.get("tracedecay"))
            .is_some();
    if has_tracedecay {
        uninstall_mcp_server(&mcp_path);
    }
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
