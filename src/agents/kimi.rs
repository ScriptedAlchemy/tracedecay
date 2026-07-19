// Rust guideline compliant 2025-10-17
//! Moonshot Kimi CLI agent integration.
//!
//! Two install surfaces are maintained side by side:
//! - Legacy Kimi CLI: registers the tracedecay MCP server in `~/.kimi/mcp.json`
//!   (standard `mcpServers` JSON schema, same shape as Claude/Cursor) and
//!   appends prompt rules to `~/.kimi/AGENTS.md`. Kimi has no hook system and
//!   no per-tool auto-approval — approval is handled globally via Kimi's
//!   YOLO / AFK modes.
//! - Kimi Code CLI native plugin: deploys the tracedecay plugin bundle to
//!   `<kimi-code-home>/plugins/managed/tracedecay/` and registers it in
//!   `<kimi-code-home>/plugins/installed.json`, where `<kimi-code-home>` is
//!   `$KIMI_CODE_HOME` when set, else `~/.kimi-code`. The plugin manifest owns
//!   the MCP server, skills, and commands for the new CLI, so a stale direct
//!   registration in `<kimi-code-home>/mcp.json` is migrated away on install.

use std::path::{Path, PathBuf};

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

/// Moonshot Kimi CLI agent.
pub struct KimiIntegration;

impl AgentIntegration for KimiIntegration {
    fn name(&self) -> &'static str {
        "Kimi CLI"
    }

    fn id(&self) -> &'static str {
        "kimi"
    }

    fn install(&self, ctx: &InstallContext) -> Result<()> {
        let kimi_dir = ctx.home.join(".kimi");
        std::fs::create_dir_all(&kimi_dir).ok();

        let mcp_path = kimi_dir.join("mcp.json");
        install_mcp_server(&mcp_path, &ctx.tracedecay_bin)?;

        let agents_md = kimi_dir.join("AGENTS.md");
        install_prompt_rules(&agents_md)?;
        super::install_managed_skill_prompt_index(
            &ctx.home,
            &agents_md,
            crate::automation::skill_targets::SkillInstallTarget::Kimi,
        )?;

        let code_home = kimi_code_home(&ctx.home);
        let managed_dir = deploy_kimi_plugin(&code_home, &ctx.tracedecay_bin)?;
        upsert_kimi_installed_entry(&code_home)?;
        migrate_kimi_code_mcp_json(&code_home);

        eprintln!();
        eprintln!("Setup complete. Next steps:");
        eprintln!("  1. cd into your project and run: tracedecay init");
        eprintln!("  2. Start a new Kimi session — tracedecay tools are now available");
        eprintln!(
            "  3. Kimi Code CLI loads the tracedecay plugin from {}",
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
        let managed_dir = deploy_kimi_plugin(&code_home, &ctx.tracedecay_bin)?;
        upsert_kimi_installed_entry(&code_home)?;
        Ok(UpdatePluginOutcome::Refreshed(vec![managed_dir]))
    }

    fn uninstall(&self, ctx: &InstallContext) -> Result<()> {
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
        remove_kimi_installed_entry(&code_home);
        remove_kimi_plugin_dir(&code_home)?;

        eprintln!();
        eprintln!("Uninstall complete. Tracedecay has been removed from Kimi CLI.");
        eprintln!("Start a new Kimi session for changes to take effect.");
        Ok(())
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mKimi CLI integration\x1b[0m");
        let kimi_dir = ctx.home.join(".kimi");
        doctor_check_mcp(dc, &kimi_dir.join("mcp.json"));
        doctor_check_prompt(dc, &kimi_dir);
        doctor_check_plugin(dc, &kimi_code_home(&ctx.home));
    }

    fn is_detected(&self, home: &Path) -> bool {
        home.join(".kimi").is_dir()
    }

    fn primary_config_path(&self, home: &Path) -> Option<std::path::PathBuf> {
        Some(home.join(".kimi/mcp.json"))
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
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

    fn export_managed_skills(
        &self,
        home: &Path,
        profile_root: &Path,
    ) -> Result<Vec<crate::automation::skill_targets::SkillInstallSummary>> {
        let agents_md = home.join(".kimi").join("AGENTS.md");
        if !self.has_tracedecay(home) || !agents_md.exists() {
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
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".kimi-code"))
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
    for (relative, contents) in super::plugin_bundle::kimi_files() {
        let rendered = if relative == KIMI_PLUGIN_MANIFEST_RELATIVE {
            let stamped = super::plugin_bundle::stamp_manifest_version(contents)?;
            super::plugin_bundle::set_mcp_command(&stamped, tracedecay_bin)?
        } else {
            contents.to_string()
        };
        safe_write_text_file(&managed_dir.join(relative), &rendered, None)?;
    }
    eprintln!(
        "\x1b[32m✔\x1b[0m Installed Kimi Code CLI plugin at {}",
        managed_dir.display()
    );
    Ok(managed_dir)
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
    let existing = plugins.iter().position(|entry| {
        entry.get("id").and_then(|value| value.as_str()) == Some(KIMI_PLUGIN_ID)
    });
    let (enabled, installed_at) = existing
        .map(|index| &plugins[index])
        .map(|entry| {
            (
                entry
                    .get("enabled")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(true),
                entry
                    .get("installedAt")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| now.clone()),
            )
        })
        .unwrap_or_else(|| (true, now.clone()));
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
    plugins.retain(|entry| {
        entry.get("id").and_then(|value| value.as_str()) != Some(KIMI_PLUGIN_ID)
    });
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

/// Register tracedecay under `mcpServers` in `~/.kimi/mcp.json`.
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

/// Remove tracedecay from `~/.kimi/mcp.json`.
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

/// Check `~/.kimi/mcp.json` has tracedecay registered.
fn doctor_check_mcp(dc: &mut DoctorCounters, mcp_path: &Path) {
    if !mcp_path.exists() {
        dc.warn(&format!(
            "{} not found — run `tracedecay install --agent kimi` if you use Kimi CLI",
            mcp_path.display()
        ));
        return;
    }
    let settings = load_json_file(mcp_path);
    let server = settings.get("mcpServers").and_then(|v| v.get("tracedecay"));
    if server.and_then(|v| v.as_object()).is_some() {
        dc.pass(&format!("MCP server registered in {}", mcp_path.display()));
    } else {
        dc.fail(&format!(
            "MCP server NOT registered in {} — run `tracedecay install --agent kimi`",
            mcp_path.display()
        ));
    }
}

/// Check AGENTS.md contains tracedecay rules.
fn doctor_check_prompt(dc: &mut DoctorCounters, kimi_dir: &Path) {
    let agents_md = kimi_dir.join("AGENTS.md");
    if agents_md.exists() {
        let has_rules = std::fs::read_to_string(&agents_md)
            .unwrap_or_default()
            .contains("tracedecay");
        if has_rules {
            dc.pass("AGENTS.md contains tracedecay rules");
        } else {
            dc.fail("AGENTS.md missing tracedecay rules — run `tracedecay install --agent kimi`");
        }
    } else {
        dc.warn("~/.kimi/AGENTS.md does not exist");
    }
}

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
    let parses = std::fs::read_to_string(&manifest_path)
        .ok()
        .and_then(|contents| serde_json::from_str::<serde_json::Value>(&contents).ok())
        .is_some();
    if parses {
        dc.pass(&format!(
            "Kimi Code CLI plugin manifest parses at {}",
            manifest_path.display()
        ));
    } else {
        dc.fail(&format!(
            "Kimi Code CLI plugin manifest missing or invalid at {} — run `tracedecay install --agent kimi`",
            manifest_path.display()
        ));
    }
}
