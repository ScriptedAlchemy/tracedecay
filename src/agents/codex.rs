// Rust guideline compliant 2025-10-17
//! `OpenAI` Codex CLI agent integration.
//!
//! Handles registration of the tracedecay MCP server in Codex's config
//! file (`~/.codex/config.toml`), per-tool auto-approval settings, prompt
//! rules via `AGENTS.md`, and lifecycle hooks via `hooks.json`.
//!
//! Codex supports a Claude-style lifecycle hook system (`SessionStart`,
//! `UserPromptSubmit`, `SubagentStart`, `PostToolUse`, …). Hooks are enabled by
//! default, but non-managed command hooks must be reviewed and trusted with the
//! `/hooks` CLI before they run — newly installed or changed hooks are skipped
//! until trusted. The installer prints that guidance after writing `hooks.json`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::errors::{Result, TraceDecayError};

use super::{
    load_json_file, load_json_file_strict, load_toml_file, safe_write_json_file,
    safe_write_text_file, tool_names, write_toml_file, AgentIntegration, DoctorCounters,
    HealthcheckContext, InstallContext, InstallScope, UpdatePluginOutcome,
};

/// `OpenAI` Codex CLI agent.
pub struct CodexIntegration;

impl AgentIntegration for CodexIntegration {
    fn name(&self) -> &'static str {
        "Codex CLI"
    }

    fn id(&self) -> &'static str {
        "codex"
    }

    fn install(&self, ctx: &InstallContext) -> Result<()> {
        install_codex_plugin(&ctx.home, &ctx.tracedecay_bin)?;
        sweep_legacy_global_codex_config(&ctx.home);

        eprintln!();
        eprintln!("Setup complete. Next steps:");
        eprintln!("  1. cd into your project and run: tracedecay init");
        eprintln!("  2. In Codex, run: codex plugin add tracedecay@personal");
        eprintln!("  3. Start a new Codex session — tracedecay tools are now available");
        print_hook_trust_guidance();
        Ok(())
    }

    fn supports_local_install(&self) -> bool {
        true
    }

    fn install_local(&self, ctx: &InstallContext, project_path: &Path) -> Result<()> {
        for path in [
            codex_repo_plugin_install_dir(project_path).join(".codex-plugin/plugin.json"),
            codex_repo_plugin_install_dir(project_path).join(".mcp.json"),
            codex_repo_plugin_install_dir(project_path).join("hooks/hooks.json"),
            codex_repo_marketplace_path(project_path),
        ] {
            super::ensure_project_local_safe_path(project_path, &path)?;
        }
        install_codex_repo_plugin(&ctx.home, project_path, &ctx.tracedecay_bin)?;
        sweep_legacy_project_codex_config(project_path);
        print_hook_trust_guidance();
        Ok(())
    }

    fn uninstall(&self, ctx: &InstallContext) -> Result<()> {
        let codex_dir = ctx.home.join(".codex");
        let config_path = codex_dir.join("config.toml");

        uninstall_mcp_server(&config_path)?;
        uninstall_codex_plugin(&ctx.home)?;

        let agents_md = codex_dir.join("AGENTS.md");
        uninstall_prompt_rules(&agents_md);

        uninstall_hooks(&codex_dir.join("hooks.json"));
        uninstall_codex_repo_plugin_if_present(ctx)?;

        eprintln!();
        eprintln!("Uninstall complete. TraceDecay has been removed from Codex CLI.");
        eprintln!("Start a new Codex session for changes to take effect.");
        Ok(())
    }

    fn update_plugin(&self, ctx: &InstallContext) -> Result<UpdatePluginOutcome> {
        let cached_dirs = codex_plugin_cached_install_dirs(&ctx.home);
        let plugin_dir = codex_plugin_install_dir(&ctx.home);
        let mut refreshed = Vec::new();
        if !cached_dirs.is_empty() {
            let target = install_codex_cached_plugin(&ctx.home, &ctx.tracedecay_bin)?;
            refreshed.push(target);
            refreshed.push(install_codex_personal_bootstrap(
                &ctx.home,
                &ctx.tracedecay_bin,
            )?);
        }

        if let Some(project_path) = codex_update_project_path(ctx) {
            let repo_dir = codex_repo_plugin_install_dir(&project_path);
            if repo_dir.join(".codex-plugin/plugin.json").exists()
                && codex_plugin_dir_is_tracedecay(&repo_dir)
            {
                install_codex_plugin_bundle(
                    &repo_dir,
                    &ctx.tracedecay_bin,
                    InstallScope::ProjectLocal,
                    &ctx.home,
                )?;
                install_codex_marketplace_entry(
                    &codex_repo_marketplace_path(&project_path),
                    "local-repo",
                    "Local Repo",
                    "./plugins/tracedecay",
                )?;
                refreshed.push(repo_dir);
            }
        }

        if !refreshed.is_empty() {
            return Ok(UpdatePluginOutcome::Refreshed(refreshed));
        }

        let target = if codex_plugin_manifest_path(&ctx.home).exists() {
            Some(plugin_dir.clone())
        } else if Self::has_legacy_config_install(&ctx.home) {
            return Ok(UpdatePluginOutcome::ConfigOnly);
        } else {
            None
        };

        let Some(target) = target else {
            return Ok(UpdatePluginOutcome::NotInstalled);
        };
        install_codex_personal_bootstrap(&ctx.home, &ctx.tracedecay_bin)?;
        Ok(UpdatePluginOutcome::Refreshed(vec![target]))
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mCodex CLI integration\x1b[0m");
        let local_codex_dir = ctx.project_path.join(".codex");
        let local_plugin_dir = codex_repo_plugin_install_dir(&ctx.project_path);
        if local_plugin_dir.join(".codex-plugin/plugin.json").exists() {
            doctor_check_plugin_dir(dc, &local_plugin_dir);
            doctor_check_marketplace_entry(
                dc,
                &codex_repo_marketplace_path(&ctx.project_path),
                "repo marketplace",
                "./plugins/tracedecay",
                "tracedecay install --local --agent codex",
            );
        } else if local_codex_dir.join("config.toml").exists()
            || local_codex_dir.join("hooks.json").exists()
        {
            doctor_check_config(dc, &local_codex_dir.join("config.toml"));
            doctor_check_prompt_file(dc, &ctx.project_path.join("AGENTS.md"));
            doctor_check_hooks(dc, &local_codex_dir.join("hooks.json"));
        } else {
            doctor_check_plugin(dc, &ctx.home);
        }
    }

    fn is_detected(&self, home: &Path) -> bool {
        home.join(".codex").is_dir()
            || !codex_plugin_cached_install_dirs(home).is_empty()
            || codex_plugin_manifest_path(home).exists()
    }

    fn primary_config_path(&self, home: &Path) -> Option<std::path::PathBuf> {
        Some(codex_plugin_cached_install_dirs(home).pop().map_or_else(
            || codex_plugin_manifest_path(home),
            |dir| dir.join(".codex-plugin/plugin.json"),
        ))
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        if !codex_plugin_cached_install_dirs(home).is_empty()
            || codex_plugin_manifest_path(home).exists()
        {
            return true;
        }
        Self::has_legacy_config_install(home)
    }
}

impl CodexIntegration {
    fn has_legacy_config_install(home: &Path) -> bool {
        let config = home.join(".codex").join("config.toml");
        if !config.exists() {
            return false;
        }
        // If the file is unparseable, conservatively report "not installed"
        // so the caller treats it like a fresh install path.
        super::load_toml_file(&config).is_ok_and(|toml| {
            toml.get("mcp_servers")
                .and_then(|v| v.get("tracedecay"))
                .is_some()
        })
    }
}

// ---------------------------------------------------------------------------
// Install helpers
// ---------------------------------------------------------------------------

const CODEX_EMBEDDED_PLUGIN_FILES: &[(&str, &str)] = &[
    (
        ".codex-plugin/plugin.json",
        include_str!("../../codex-plugin/.codex-plugin/plugin.json"),
    ),
    (".mcp.json", include_str!("../../codex-plugin/.mcp.json")),
    ("README.md", include_str!("../../codex-plugin/README.md")),
    (
        "hooks/hooks.json",
        include_str!("../../codex-plugin/hooks/hooks.json"),
    ),
    // Codex auto-discovers every `SKILL.md` under the manifest `skills/` dir by
    // its `name`/`description` frontmatter. The Codex bundle mirrors the
    // model-invocable Cursor skills (`hooks::CURSOR_PLUGIN_SKILLS`) so both
    // hosts steer agents toward the same tracedecay workflows; the parity is
    // enforced by `codex_skills_match_the_cursor_source_for_parity`. The
    // Cursor-only slash dispatchers (`tracedecay-*`) and explicit-invoke memory
    // skills are intentionally omitted (Codex has no slash-command surface).
    (
        "skills/architecture-overview/SKILL.md",
        include_str!("../../codex-plugin/skills/architecture-overview/SKILL.md"),
    ),
    (
        "skills/assessing-test-coverage/SKILL.md",
        include_str!("../../codex-plugin/skills/assessing-test-coverage/SKILL.md"),
    ),
    (
        "skills/atomic-code-edits/SKILL.md",
        include_str!("../../codex-plugin/skills/atomic-code-edits/SKILL.md"),
    ),
    (
        "skills/auditing-code-safety/SKILL.md",
        include_str!("../../codex-plugin/skills/auditing-code-safety/SKILL.md"),
    ),
    (
        "skills/cleaning-up-dead-code/SKILL.md",
        include_str!("../../codex-plugin/skills/cleaning-up-dead-code/SKILL.md"),
    ),
    (
        "skills/code-health-report/SKILL.md",
        include_str!("../../codex-plugin/skills/code-health-report/SKILL.md"),
    ),
    (
        "skills/cross-branch-investigation/SKILL.md",
        include_str!("../../codex-plugin/skills/cross-branch-investigation/SKILL.md"),
    ),
    (
        "skills/curating-project-memory/SKILL.md",
        include_str!("../../codex-plugin/skills/curating-project-memory/SKILL.md"),
    ),
    (
        "skills/drafting-commit-and-pr/SKILL.md",
        include_str!("../../codex-plugin/skills/drafting-commit-and-pr/SKILL.md"),
    ),
    (
        "skills/exploring-types-and-traits/SKILL.md",
        include_str!("../../codex-plugin/skills/exploring-types-and-traits/SKILL.md"),
    ),
    (
        "skills/finding-duplicate-logic/SKILL.md",
        include_str!("../../codex-plugin/skills/finding-duplicate-logic/SKILL.md"),
    ),
    (
        "skills/finding-impacted-areas/SKILL.md",
        include_str!("../../codex-plugin/skills/finding-impacted-areas/SKILL.md"),
    ),
    (
        "skills/fixing-build-and-type-errors/SKILL.md",
        include_str!("../../codex-plugin/skills/fixing-build-and-type-errors/SKILL.md"),
    ),
    (
        "skills/inspecting-managed-skills/SKILL.md",
        include_str!("../../codex-plugin/skills/inspecting-managed-skills/SKILL.md"),
    ),
    (
        "skills/porting-code/SKILL.md",
        include_str!("../../codex-plugin/skills/porting-code/SKILL.md"),
    ),
    (
        "skills/project-status/SKILL.md",
        include_str!("../../codex-plugin/skills/project-status/SKILL.md"),
    ),
    (
        "skills/reading-code-cheaply/SKILL.md",
        include_str!("../../codex-plugin/skills/reading-code-cheaply/SKILL.md"),
    ),
    (
        "skills/recalling-project-memory/SKILL.md",
        include_str!("../../codex-plugin/skills/recalling-project-memory/SKILL.md"),
    ),
    (
        "skills/recalling-session-context/SKILL.md",
        include_str!("../../codex-plugin/skills/recalling-session-context/SKILL.md"),
    ),
    (
        "skills/refactoring-safely/SKILL.md",
        include_str!("../../codex-plugin/skills/refactoring-safely/SKILL.md"),
    ),
    (
        "skills/reviewing-a-diff/SKILL.md",
        include_str!("../../codex-plugin/skills/reviewing-a-diff/SKILL.md"),
    ),
    (
        "skills/running-impacted-tests/SKILL.md",
        include_str!("../../codex-plugin/skills/running-impacted-tests/SKILL.md"),
    ),
    (
        "skills/searching-for-code/SKILL.md",
        include_str!("../../codex-plugin/skills/searching-for-code/SKILL.md"),
    ),
    (
        "skills/tracing-functions/SKILL.md",
        include_str!("../../codex-plugin/skills/tracing-functions/SKILL.md"),
    ),
    (
        "skills/tracking-session-health/SKILL.md",
        include_str!("../../codex-plugin/skills/tracking-session-health/SKILL.md"),
    ),
    (
        "skills/using-the-cli/SKILL.md",
        include_str!("../../codex-plugin/skills/using-the-cli/SKILL.md"),
    ),
];

fn codex_plugin_install_dir(home: &Path) -> PathBuf {
    home.join("plugins/tracedecay")
}

fn codex_plugin_cached_root(home: &Path) -> PathBuf {
    home.join(".codex/plugins/cache/personal/tracedecay")
}

fn codex_plugin_current_cached_install_dir(home: &Path) -> PathBuf {
    codex_plugin_cached_root(home).join(env!("CARGO_PKG_VERSION"))
}

fn codex_plugin_cached_install_dirs(home: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(codex_plugin_cached_root(home)) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.is_dir() && codex_plugin_dir_is_tracedecay(path))
        .collect();
    dirs.sort();
    dirs
}

fn codex_plugin_manifest_path(home: &Path) -> PathBuf {
    codex_plugin_install_dir(home).join(".codex-plugin/plugin.json")
}

fn codex_personal_marketplace_path(home: &Path) -> PathBuf {
    home.join(".agents/plugins/marketplace.json")
}

fn codex_repo_plugin_install_dir(project_path: &Path) -> PathBuf {
    project_path.join("plugins/tracedecay")
}

fn codex_repo_marketplace_path(project_path: &Path) -> PathBuf {
    project_path.join(".agents/plugins/marketplace.json")
}

fn codex_update_project_path(ctx: &InstallContext) -> Option<PathBuf> {
    ctx.project_root
        .clone()
        .or_else(|| std::env::current_dir().ok())
}

fn install_codex_plugin(home: &Path, tracedecay_bin: &str) -> Result<()> {
    let cached_dirs = codex_plugin_cached_install_dirs(home);
    if !cached_dirs.is_empty() {
        let install_dir = install_codex_cached_plugin(home, tracedecay_bin)?;
        install_codex_personal_bootstrap(home, tracedecay_bin)?;
        eprintln!(
            "\x1b[32m✔\x1b[0m Refreshed installed Codex plugin bundle at {}",
            install_dir.display()
        );
        return Ok(());
    }

    let install_dir = install_codex_personal_bootstrap(home, tracedecay_bin)?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Installed Codex plugin source at {}",
        install_dir.display()
    );
    Ok(())
}

fn install_codex_personal_bootstrap(home: &Path, tracedecay_bin: &str) -> Result<PathBuf> {
    let install_dir = codex_plugin_install_dir(home);
    install_codex_plugin_bundle(&install_dir, tracedecay_bin, InstallScope::Global, home)?;
    install_codex_marketplace_entry(
        &codex_personal_marketplace_path(home),
        "personal",
        "Personal",
        "./plugins/tracedecay",
    )?;
    Ok(install_dir)
}

fn install_codex_cached_plugin(home: &Path, tracedecay_bin: &str) -> Result<PathBuf> {
    let target = codex_plugin_current_cached_install_dir(home);
    install_codex_plugin_bundle(&target, tracedecay_bin, InstallScope::Global, home)?;
    for stale_dir in codex_plugin_cached_install_dirs(home) {
        if stale_dir != target {
            remove_codex_plugin_install(&stale_dir)?;
        }
    }
    Ok(target)
}

fn install_codex_repo_plugin(home: &Path, project_path: &Path, tracedecay_bin: &str) -> Result<()> {
    let install_dir = codex_repo_plugin_install_dir(project_path);
    install_codex_plugin_bundle(
        &install_dir,
        tracedecay_bin,
        InstallScope::ProjectLocal,
        home,
    )?;
    install_codex_marketplace_entry(
        &codex_repo_marketplace_path(project_path),
        "local-repo",
        "Local Repo",
        "./plugins/tracedecay",
    )?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Installed Codex repo plugin source at {}",
        install_dir.display()
    );
    Ok(())
}

fn sweep_legacy_global_codex_config(home: &Path) {
    let codex_dir = home.join(".codex");
    uninstall_tracedecay_mcp_if_present(&codex_dir.join("config.toml"));
    uninstall_hooks(&codex_dir.join("hooks.json"));
    uninstall_prompt_rules(&codex_dir.join("AGENTS.md"));
}

fn sweep_legacy_project_codex_config(project_path: &Path) {
    let codex_dir = project_path.join(".codex");
    uninstall_tracedecay_mcp_if_present(&codex_dir.join("config.toml"));
    uninstall_hooks(&codex_dir.join("hooks.json"));
}

/// Directory of the Codex-native scheduled automation that tracedecay
/// v0.0.10 through v0.0.20 installed with `install --agent codex --automation`.
const LEGACY_CODEX_NATIVE_AUTOMATION_ID: &str = "watch-tracedecay-memory";

/// Removes the legacy Codex-native scheduled automation, returning whether one
/// was present. The `TraceDecay` daemon scheduler replaced it; leaving the
/// record in place would run both schedulers concurrently after an upgrade.
pub fn remove_legacy_codex_native_automation(home: &Path) -> Result<bool> {
    let automation_dir = home
        .join(".codex/automations")
        .join(LEGACY_CODEX_NATIVE_AUTOMATION_ID);
    match std::fs::remove_dir_all(&automation_dir) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(TraceDecayError::Config {
            message: format!(
                "failed to remove legacy Codex automation {}: {e}",
                automation_dir.display()
            ),
        }),
    }
}

fn uninstall_tracedecay_mcp_if_present(config_path: &Path) {
    let Ok(contents) = std::fs::read_to_string(config_path) else {
        return;
    };
    if !contents.contains("tracedecay") {
        return;
    }
    if let Err(err) = uninstall_mcp_server(config_path) {
        eprintln!(
            "  Could not remove project-local Codex MCP config from {}: {err}",
            config_path.display()
        );
    }
}

fn install_codex_plugin_bundle(
    install_dir: &Path,
    tracedecay_bin: &str,
    scope: InstallScope,
    profile_home: &Path,
) -> Result<()> {
    write_codex_plugin_bundle_base(install_dir, tracedecay_bin, scope)?;
    install_codex_managed_skill_overlay(profile_home, install_dir).map(|_| ())
}

/// Export a complete shareable Codex plugin bundle with active managed skills.
pub fn export_codex_plugin_artifact(
    profile_root: &Path,
    output: &Path,
    tracedecay_bin: &str,
) -> Result<crate::automation::skill_targets::SkillInstallSummary> {
    write_codex_plugin_bundle_base(output, tracedecay_bin, InstallScope::Global)?;
    crate::automation::skill_targets::export_native_skill_overlay(
        profile_root,
        crate::automation::skill_targets::SkillInstallTarget::Codex,
        output,
    )
}

fn write_codex_plugin_bundle_base(
    install_dir: &Path,
    tracedecay_bin: &str,
    scope: InstallScope,
) -> Result<()> {
    if let Some(parent) = install_dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| TraceDecayError::Config {
            message: format!("failed to create {}: {e}", parent.display()),
        })?;
    }
    remove_codex_plugin_install(install_dir)?;
    write_codex_plugin_files(install_dir, tracedecay_bin, scope)
}

fn install_codex_managed_skill_overlay(
    profile_home: &Path,
    install_dir: &Path,
) -> Result<crate::automation::skill_targets::SkillInstallSummary> {
    let profile_root = crate::automation::skill_targets::profile_root_for_agent_home(profile_home);
    crate::automation::skill_targets::install_managed_skills(
        &profile_root,
        crate::automation::skill_targets::SkillInstallTarget::Codex,
        install_dir,
    )
}

fn write_codex_plugin_files(
    install_dir: &Path,
    tracedecay_bin: &str,
    scope: InstallScope,
) -> Result<()> {
    for &(relative, contents) in CODEX_EMBEDDED_PLUGIN_FILES {
        let rendered = match relative {
            ".codex-plugin/plugin.json" => codex_plugin_manifest(contents)?,
            ".mcp.json" => codex_plugin_mcp(contents, tracedecay_bin, scope)?,
            "hooks/hooks.json" => codex_plugin_hooks(contents, tracedecay_bin)?,
            _ => contents.to_string(),
        };
        safe_write_text_file(&install_dir.join(relative), &rendered, None)?;
    }
    Ok(())
}

fn codex_plugin_manifest(raw: &str) -> Result<String> {
    let mut manifest: serde_json::Value = serde_json::from_str(raw)?;
    manifest["version"] = json!(env!("CARGO_PKG_VERSION"));
    Ok(format!("{}\n", serde_json::to_string_pretty(&manifest)?))
}

fn codex_plugin_mcp(raw: &str, tracedecay_bin: &str, scope: InstallScope) -> Result<String> {
    let mut mcp: serde_json::Value = serde_json::from_str(raw)?;
    let server = &mut mcp["mcpServers"]["tracedecay"];
    server["command"] = json!(tracedecay_bin);
    match scope {
        InstallScope::Global => {
            server["args"] = json!(["serve"]);
            server["env"] = json!({ "TRACEDECAY_ENABLE_GLOBAL_DB": "1" });
        }
        InstallScope::ProjectLocal => {
            server["args"] = json!(["serve", "--path", "."]);
            if let Some(object) = server.as_object_mut() {
                object.remove("env");
            }
        }
    }
    Ok(format!("{}\n", serde_json::to_string_pretty(&mcp)?))
}

fn codex_plugin_hooks(raw: &str, tracedecay_bin: &str) -> Result<String> {
    let mut hooks: serde_json::Value = serde_json::from_str(raw)?;
    install_codex_hook_event(
        &mut hooks,
        "SessionStart",
        tracedecay_bin,
        "hook-codex-session-start",
        5,
        None,
    );
    install_codex_hook_event(
        &mut hooks,
        "UserPromptSubmit",
        tracedecay_bin,
        "hook-codex-user-prompt-submit",
        5,
        None,
    );
    install_codex_hook_event(
        &mut hooks,
        "SubagentStart",
        tracedecay_bin,
        "hook-codex-subagent-start",
        5,
        None,
    );
    install_codex_hook_event(
        &mut hooks,
        "PostToolUse",
        tracedecay_bin,
        "hook-codex-post-tool-use",
        60,
        Some("Bash|apply_patch"),
    );
    install_codex_hook_event(
        &mut hooks,
        "PostCompact",
        tracedecay_bin,
        "hook-codex-post-compact",
        120,
        Some("auto|manual"),
    );
    Ok(format!("{}\n", serde_json::to_string_pretty(&hooks)?))
}

fn install_codex_marketplace_entry(
    marketplace_path: &Path,
    marketplace_name: &str,
    display_name: &str,
    source_path: &str,
) -> Result<()> {
    let mut marketplace = load_json_file_strict(marketplace_path)?;
    if !marketplace.is_object() {
        marketplace = json!({});
    }
    if marketplace
        .get("name")
        .and_then(|value| value.as_str())
        .is_none()
    {
        marketplace["name"] = json!(marketplace_name);
    }
    if !marketplace
        .get("interface")
        .is_some_and(serde_json::Value::is_object)
    {
        marketplace["interface"] = json!({ "displayName": display_name });
    } else if marketplace["interface"]
        .get("displayName")
        .and_then(|value| value.as_str())
        .is_none()
    {
        marketplace["interface"]["displayName"] = json!(display_name);
    }
    if !marketplace
        .get("plugins")
        .is_some_and(serde_json::Value::is_array)
    {
        marketplace["plugins"] = json!([]);
    }
    let Some(plugins) = marketplace["plugins"].as_array_mut() else {
        return Err(TraceDecayError::Config {
            message: "failed to normalize Codex marketplace plugins to an array".to_string(),
        });
    };
    plugins.retain(|entry| {
        !matches!(
            entry.get("name").and_then(|value| value.as_str()),
            Some("tracedecay")
        )
    });
    plugins.push(json!({
        "name": "tracedecay",
        "source": {
            "source": "local",
            "path": source_path,
        },
        "policy": {
            "installation": "AVAILABLE",
            "authentication": "ON_INSTALL",
        },
        "category": "Productivity",
    }));
    safe_write_json_file(marketplace_path, &marketplace, None)?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Added tracedecay to Codex {marketplace_name} marketplace at {}",
        marketplace_path.display()
    );
    Ok(())
}

fn uninstall_codex_plugin(home: &Path) -> Result<()> {
    for install_dir in codex_plugin_cached_install_dirs(home) {
        remove_codex_plugin_bootstrap_source(&install_dir)?;
    }
    remove_codex_plugin_bootstrap_source(&codex_plugin_install_dir(home))?;
    remove_codex_marketplace_entry(home)?;
    Ok(())
}

fn uninstall_codex_repo_plugin_if_present(ctx: &InstallContext) -> Result<()> {
    let Some(project_path) = codex_update_project_path(ctx) else {
        return Ok(());
    };
    let install_dir = codex_repo_plugin_install_dir(&project_path);
    if install_dir.join(".codex-plugin/plugin.json").exists()
        && codex_plugin_dir_is_tracedecay(&install_dir)
    {
        remove_codex_plugin_install(&install_dir)?;
    }
    remove_codex_marketplace_entry_at(&codex_repo_marketplace_path(&project_path), "repo")?;
    Ok(())
}

fn remove_codex_plugin_bootstrap_source(install_dir: &Path) -> Result<()> {
    if install_dir.exists() && codex_plugin_dir_is_tracedecay(install_dir) {
        remove_codex_plugin_skills_dir(install_dir)?;
    }
    remove_codex_plugin_install(install_dir)
}

fn remove_codex_plugin_skills_dir(install_dir: &Path) -> Result<()> {
    let skills_dir = install_dir.join("skills");
    let Ok(metadata) = std::fs::symlink_metadata(&skills_dir) else {
        return Ok(());
    };
    if metadata.file_type().is_symlink() || metadata.is_file() {
        std::fs::remove_file(&skills_dir).map_err(|e| TraceDecayError::Config {
            message: format!("failed to remove {}: {e}", skills_dir.display()),
        })?;
    } else if metadata.is_dir() {
        remove_codex_managed_skill_overlay(install_dir);
        remove_codex_plugin_managed_skills(install_dir, &skills_dir)?;
    }
    Ok(())
}

fn remove_codex_managed_skill_overlay(install_dir: &Path) {
    std::fs::remove_dir_all(install_dir.join("skills/agent-managed")).ok();
}

fn remove_codex_plugin_managed_skills(install_dir: &Path, skills_dir: &Path) -> Result<()> {
    let managed: HashSet<PathBuf> = codex_plugin_managed_paths(install_dir)
        .into_iter()
        .filter(|path| path.starts_with(skills_dir))
        .collect();
    let mut files = collect_regular_files(skills_dir).map_err(|e| TraceDecayError::Config {
        message: format!("failed to list {}: {e}", skills_dir.display()),
    })?;
    files.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for file in files {
        if managed.contains(&file) || codex_skill_file_is_legacy_tracedecay_managed(&file) {
            std::fs::remove_file(&file).map_err(|e| TraceDecayError::Config {
                message: format!("failed to remove {}: {e}", file.display()),
            })?;
        }
    }
    prune_empty_dirs(skills_dir).map_err(|e| TraceDecayError::Config {
        message: format!("failed to prune empty Codex skill directories: {e}"),
    })
}

fn codex_skill_file_is_legacy_tracedecay_managed(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "SKILL.md")
        && std::fs::read_to_string(path).is_ok_and(|contents| {
            contents
                .lines()
                .any(|line| line.starts_with("name: tracedecay:"))
        })
}

fn prune_empty_dirs(root: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            prune_empty_dirs(&entry.path())?;
        }
    }
    if std::fs::read_dir(root)?.next().is_none() {
        std::fs::remove_dir(root)?;
    }
    Ok(())
}

fn remove_codex_plugin_install(install_dir: &Path) -> Result<()> {
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
                "refusing to replace non-directory Codex plugin path {}",
                install_dir.display()
            ),
        });
    }
    if !codex_plugin_dir_is_tracedecay(install_dir) {
        return Err(TraceDecayError::Config {
            message: format!(
                "refusing to replace unmanaged Codex plugin directory {}",
                install_dir.display()
            ),
        });
    }
    remove_codex_plugin_skills_dir(install_dir)?;
    if codex_plugin_dir_has_only_managed_files(install_dir) {
        std::fs::remove_dir_all(install_dir).map_err(|e| TraceDecayError::Config {
            message: format!("failed to remove {}: {e}", install_dir.display()),
        })?;
    } else {
        for path in codex_plugin_managed_paths(install_dir) {
            std::fs::remove_file(&path).ok();
        }
    }
    Ok(())
}

fn codex_plugin_dir_is_tracedecay(install_dir: &Path) -> bool {
    let manifest = load_json_file(&install_dir.join(".codex-plugin/plugin.json"));
    matches!(
        manifest.get("name").and_then(|value| value.as_str()),
        Some("tracedecay")
    )
}

fn codex_plugin_dir_has_only_managed_files(install_dir: &Path) -> bool {
    let Ok(entries) = collect_regular_files(install_dir) else {
        return false;
    };
    let managed = codex_plugin_managed_paths(install_dir);
    entries.iter().all(|entry| managed.contains(entry))
}

fn codex_plugin_managed_paths(install_dir: &Path) -> Vec<PathBuf> {
    CODEX_EMBEDDED_PLUGIN_FILES
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

fn remove_codex_marketplace_entry(home: &Path) -> Result<()> {
    let marketplace_path = codex_personal_marketplace_path(home);
    remove_codex_marketplace_entry_at(&marketplace_path, "personal")
}

fn remove_codex_marketplace_entry_at(marketplace_path: &Path, label: &str) -> Result<()> {
    if !marketplace_path.exists() {
        return Ok(());
    }
    let mut marketplace = load_json_file_strict(marketplace_path)?;
    let Some(plugins) = marketplace
        .get_mut("plugins")
        .and_then(|value| value.as_array_mut())
    else {
        return Ok(());
    };
    let before = plugins.len();
    plugins.retain(|entry| {
        !matches!(
            entry.get("name").and_then(|value| value.as_str()),
            Some("tracedecay")
        )
    });
    if plugins.len() == before {
        return Ok(());
    }
    safe_write_json_file(marketplace_path, &marketplace, None)?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Removed tracedecay from Codex {label} marketplace at {}",
        marketplace_path.display()
    );
    Ok(())
}

/// Insert (or reconcile) the tracedecay-owned matcher group for `event`.
///
/// Drops any pre-existing group that already contains our `subcommand` handler
/// (so refinements to matcher/timeout reach old configs) while preserving every
/// foreign group. Idempotent: exactly one tracedecay group per event.
fn install_codex_hook_event(
    hooks: &mut serde_json::Value,
    event: &str,
    tracedecay_bin: &str,
    subcommand: &str,
    timeout: u64,
    matcher: Option<&str>,
) {
    let existing = hooks["hooks"][event]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut groups: Vec<serde_json::Value> = existing
        .into_iter()
        .filter(|group| !group_has_subcommand(group, subcommand))
        .collect();

    let handler = json!({
        "type": "command",
        "command": super::hook_command(tracedecay_bin, subcommand),
        "timeout": timeout,
    });
    let mut group = json!({ "hooks": [handler] });
    if let Some(matcher) = matcher {
        group["matcher"] = json!(matcher);
    }
    groups.push(group);

    hooks["hooks"][event] = serde_json::Value::Array(groups);
}

/// True when any handler command in `group` contains `subcommand`.
fn group_has_subcommand(group: &serde_json::Value, subcommand: &str) -> bool {
    group["hooks"].as_array().is_some_and(|handlers| {
        handlers.iter().any(|h| {
            h.get("command")
                .and_then(|c| c.as_str())
                .is_some_and(|command| command.contains(subcommand))
        })
    })
}

/// Codex requires non-managed command hooks to be trusted via `/hooks` before
/// they run; newly installed/changed hooks are skipped until trusted.
fn print_hook_trust_guidance() {
    eprintln!();
    eprintln!(
        "\x1b[1mAction required:\x1b[0m Codex skips new/changed command hooks until you trust them."
    );
    eprintln!("  Run \x1b[1m/hooks\x1b[0m inside Codex to review and trust the tracedecay hooks.");
    eprintln!(
        "  (For one-off non-interactive runs you can pass --dangerously-bypass-hook-trust, \
         but trusting via /hooks is recommended.)"
    );
}

// ---------------------------------------------------------------------------
// Uninstall helpers
// ---------------------------------------------------------------------------

/// Remove tracedecay-owned hook groups from a Codex `hooks.json`.
fn uninstall_hooks(hooks_path: &Path) {
    const SUBCOMMANDS: [&str; 6] = [
        "hook-codex-session-start",
        "hook-codex-user-prompt-submit",
        "hook-codex-subagent-start",
        "hook-codex-post-tool-use",
        "hook-codex-post-compact",
        "hook-codex-pre-tool-use",
    ];

    if !hooks_path.exists() {
        return;
    }
    let Ok(mut hooks) = load_json_file_strict(hooks_path) else {
        return;
    };

    let Some(events) = hooks.get_mut("hooks").and_then(|h| h.as_object_mut()) else {
        return;
    };
    for groups in events.values_mut() {
        if let Some(arr) = groups.as_array_mut() {
            arr.retain(|group| !SUBCOMMANDS.iter().any(|sc| group_has_subcommand(group, sc)));
        }
    }
    events.retain(|_, groups| groups.as_array().is_some_and(|a| !a.is_empty()));

    let is_empty = hooks
        .get("hooks")
        .and_then(|h| h.as_object())
        .is_some_and(serde_json::Map::is_empty);
    if is_empty {
        std::fs::remove_file(hooks_path).ok();
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed {} (was empty)",
            hooks_path.display()
        );
    } else if safe_write_json_file(hooks_path, &hooks, None).is_ok() {
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed tracedecay hooks from {}",
            hooks_path.display()
        );
    }
}

/// Remove MCP server from ~/.codex/config.toml.
fn uninstall_mcp_server(config_path: &Path) -> Result<()> {
    if !config_path.exists() {
        return Ok(());
    }
    let mut config = load_toml_file(config_path)?;
    let Some(table) = config.as_table_mut() else {
        return Ok(());
    };
    let Some(servers) = table.get_mut("mcp_servers").and_then(|v| v.as_table_mut()) else {
        return Ok(());
    };
    let removed = servers.remove("tracedecay").is_some();
    if !removed {
        eprintln!(
            "  No tracedecay MCP server in {}, skipping",
            config_path.display()
        );
        return Ok(());
    }
    if servers.is_empty() {
        table.remove("mcp_servers");
    }
    if table.is_empty() {
        std::fs::remove_file(config_path).ok();
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed {} (was empty)",
            config_path.display()
        );
    } else {
        write_toml_file(config_path, &config)?;
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed tracedecay MCP server from {}",
            config_path.display()
        );
    }
    Ok(())
}

/// Remove tracedecay rules from AGENTS.md.
fn uninstall_prompt_rules(agents_md: &Path) {
    if !agents_md.exists() {
        return;
    }
    let Ok(contents) = std::fs::read_to_string(agents_md) else {
        return;
    };
    if !contents.contains("tracedecay") {
        eprintln!("  AGENTS.md does not contain tracedecay rules, skipping");
        return;
    }
    let marker_new = "## Prefer tracedecay MCP tools";
    let (marker, start) = if let Some(start) = contents.find(marker_new) {
        (marker_new, start)
    } else {
        return;
    };
    let after_marker = start + marker.len();
    let end = contents[after_marker..]
        .find("\n## ")
        .map_or(contents.len(), |pos| after_marker + pos);
    let mut new_contents = String::new();
    new_contents.push_str(contents[..start].trim_end());
    let remainder = &contents[end..];
    if !remainder.is_empty() {
        new_contents.push_str("\n\n");
        new_contents.push_str(remainder.trim_start());
    }
    let new_contents = new_contents.trim().to_string();
    if new_contents.is_empty() {
        std::fs::remove_file(agents_md).ok();
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed {} (was empty)",
            agents_md.display()
        );
    } else {
        std::fs::write(agents_md, format!("{new_contents}\n")).ok();
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed tracedecay rules from {}",
            agents_md.display()
        );
    }
}

// ---------------------------------------------------------------------------
// Healthcheck helpers
// ---------------------------------------------------------------------------

fn doctor_check_plugin(dc: &mut DoctorCounters, home: &Path) {
    let cached_dirs = codex_plugin_cached_install_dirs(home);
    if !cached_dirs.is_empty() {
        for plugin_dir in cached_dirs {
            doctor_check_plugin_dir(dc, &plugin_dir);
        }
        return;
    }

    let plugin_dir = codex_plugin_install_dir(home);
    let manifest_path = plugin_dir.join(".codex-plugin/plugin.json");
    if !manifest_path.exists() {
        if CodexIntegration::has_legacy_config_install(home) {
            doctor_check_config(dc, &home.join(".codex/config.toml"));
            dc.warn(
                "Codex uses a legacy config-managed tracedecay install — run `tracedecay install --agent codex` to install the Codex plugin bundle",
            );
        } else {
            dc.warn(&format!(
                "{} not found — run `tracedecay install --agent codex` if you use Codex CLI",
                manifest_path.display()
            ));
        }
        return;
    }

    let manifest = load_json_file(&manifest_path);
    if manifest.get("name").and_then(|value| value.as_str()) == Some("tracedecay") {
        dc.pass(&format!(
            "Codex plugin manifest present in {}",
            manifest_path.display()
        ));
    } else {
        dc.fail(&format!(
            "Codex plugin manifest at {} is not a tracedecay plugin",
            manifest_path.display()
        ));
    }
    match manifest.get("version").and_then(|value| value.as_str()) {
        Some(env!("CARGO_PKG_VERSION")) => dc.pass("Codex plugin version matches tracedecay"),
        Some(version) => dc.warn(&format!(
            "Codex plugin version {version} does not match tracedecay {} — run `tracedecay update-plugin`",
            env!("CARGO_PKG_VERSION")
        )),
        None => dc.warn("Codex plugin manifest does not contain a version"),
    }

    let mcp_path = plugin_dir.join(".mcp.json");
    let mcp = load_json_file(&mcp_path);
    if mcp
        .get("mcpServers")
        .and_then(|servers| servers.get("tracedecay"))
        .is_some()
    {
        dc.pass(&format!(
            "Codex plugin MCP server registered in {}",
            mcp_path.display()
        ));
    } else {
        dc.fail(&format!(
            "Codex plugin MCP server missing in {} — run `tracedecay install --agent codex`",
            mcp_path.display()
        ));
    }
    doctor_check_hooks(dc, &plugin_dir.join("hooks/hooks.json"));

    doctor_check_marketplace_entry(
        dc,
        &codex_personal_marketplace_path(home),
        "personal marketplace",
        "./plugins/tracedecay",
        "tracedecay install --agent codex",
    );
}

fn doctor_check_marketplace_entry(
    dc: &mut DoctorCounters,
    marketplace_path: &Path,
    label: &str,
    expected_source_path: &str,
    install_command: &str,
) {
    let marketplace = load_json_file(marketplace_path);
    let has_entry = marketplace
        .get("plugins")
        .and_then(|value| value.as_array())
        .is_some_and(|plugins| {
            plugins.iter().any(|entry| {
                entry.get("name").and_then(|value| value.as_str()) == Some("tracedecay")
                    && entry
                        .get("source")
                        .and_then(|source| source.get("source"))
                        .and_then(|value| value.as_str())
                        == Some("local")
                    && entry
                        .get("source")
                        .and_then(|source| source.get("path"))
                        .and_then(|value| value.as_str())
                        == Some(expected_source_path)
            })
        });
    if has_entry {
        dc.pass(&format!(
            "Codex {label} contains tracedecay in {}",
            marketplace_path.display()
        ));
    } else {
        dc.warn(&format!(
            "Codex {label} missing tracedecay in {} — run `{install_command}`",
            marketplace_path.display()
        ));
    }
}

fn doctor_check_plugin_dir(dc: &mut DoctorCounters, plugin_dir: &Path) {
    let manifest_path = plugin_dir.join(".codex-plugin/plugin.json");
    let manifest = load_json_file(&manifest_path);
    if manifest.get("name").and_then(|value| value.as_str()) == Some("tracedecay") {
        dc.pass(&format!(
            "Codex plugin manifest present in {}",
            manifest_path.display()
        ));
    } else {
        dc.fail(&format!(
            "Codex plugin manifest at {} is not a tracedecay plugin",
            manifest_path.display()
        ));
    }
    match manifest.get("version").and_then(|value| value.as_str()) {
        Some(env!("CARGO_PKG_VERSION")) => dc.pass("Codex plugin version matches tracedecay"),
        Some(version) => dc.warn(&format!(
            "Codex plugin version {version} does not match tracedecay {} — run `tracedecay update-plugin`",
            env!("CARGO_PKG_VERSION")
        )),
        None => dc.warn("Codex plugin manifest does not contain a version"),
    }

    let mcp_path = plugin_dir.join(".mcp.json");
    let mcp = load_json_file(&mcp_path);
    if mcp
        .get("mcpServers")
        .and_then(|servers| servers.get("tracedecay"))
        .is_some()
    {
        dc.pass(&format!(
            "Codex plugin MCP server registered in {}",
            mcp_path.display()
        ));
    } else {
        dc.fail(&format!(
            "Codex plugin MCP server missing in {} — rerun tracedecay Codex install",
            mcp_path.display()
        ));
    }
    doctor_check_hooks(dc, &plugin_dir.join("hooks/hooks.json"));
}

/// Check config.toml has tracedecay registered.
fn doctor_check_config(dc: &mut DoctorCounters, config_path: &Path) {
    if !config_path.exists() {
        dc.warn(&format!(
            "{} not found — run `tracedecay install --agent codex` if you use Codex CLI",
            config_path.display()
        ));
        return;
    }

    let config = match load_toml_file(config_path) {
        Ok(c) => c,
        Err(e) => {
            dc.fail(&format!("{e}"));
            return;
        }
    };
    let has_server = config
        .get("mcp_servers")
        .and_then(|v| v.get("tracedecay"))
        .and_then(|v| v.as_table())
        .is_some();

    if !has_server {
        dc.fail(&format!(
            "MCP server NOT registered in {} — run `tracedecay install --agent codex`",
            config_path.display()
        ));
        return;
    }
    dc.pass(&format!(
        "MCP server registered in {}",
        config_path.display()
    ));

    // Check tool auto-approval
    let tools = config
        .get("mcp_servers")
        .and_then(|v| v.get("tracedecay"))
        .and_then(|v| v.get("tools"))
        .and_then(|v| v.as_table());

    let auto_count = tools.map_or(0, |t| {
        t.values()
            .filter(|v| v.get("approval_mode").and_then(|m| m.as_str()) == Some("auto"))
            .count()
    });

    let tools = tool_names();
    let tools_len = tools.len();
    if auto_count >= tools_len {
        dc.pass(&format!("All {tools_len} tools set to auto-approve"));
    } else if auto_count > 0 {
        dc.warn(&format!(
            "{auto_count}/{tools_len} tools auto-approved — run `tracedecay install --agent codex` to update"
        ));
    } else {
        dc.warn("No tools auto-approved — Codex will prompt for each tool call");
    }
}

/// Check AGENTS.md contains tracedecay rules.
fn doctor_check_prompt_file(dc: &mut DoctorCounters, agents_md: &Path) {
    if agents_md.exists() {
        let has_rules = std::fs::read_to_string(agents_md)
            .unwrap_or_default()
            .contains("tracedecay");
        if has_rules {
            dc.pass(&format!(
                "AGENTS.md contains tracedecay rules in {}",
                agents_md.display()
            ));
        } else {
            dc.fail(&format!(
                "AGENTS.md missing tracedecay rules in {} — run `tracedecay install --local --agent codex` or `tracedecay install --agent codex`",
                agents_md.display()
            ));
        }
    } else {
        dc.warn(&format!("{} does not exist", agents_md.display()));
    }
}

/// Check hooks.json registers the tracedecay lifecycle hooks, and remind the
/// user that Codex requires trusting them via `/hooks` before they run.
fn doctor_check_hooks(dc: &mut DoctorCounters, hooks_path: &Path) {
    if !hooks_path.exists() {
        dc.warn(&format!(
            "{} not found — run `tracedecay install --agent codex` to add lifecycle hooks",
            hooks_path.display()
        ));
        return;
    }
    let hooks = super::load_json_file(hooks_path);
    let expected = [
        ("SessionStart", "hook-codex-session-start"),
        ("UserPromptSubmit", "hook-codex-user-prompt-submit"),
        ("SubagentStart", "hook-codex-subagent-start"),
        ("PostToolUse", "hook-codex-post-tool-use"),
        ("PostCompact", "hook-codex-post-compact"),
    ];
    let missing: Vec<&str> = expected
        .iter()
        .filter_map(|(event, command)| {
            (!codex_hook_present(&hooks, event, command)).then_some(*event)
        })
        .collect();
    if missing.is_empty() {
        dc.pass(&format!(
            "All {} Codex lifecycle hooks registered in {}",
            expected.len(),
            hooks_path.display()
        ));
        dc.info(
            "Codex skips new/changed command hooks until trusted — run `/hooks` in Codex to trust the tracedecay hooks",
        );
    } else {
        dc.warn(&format!(
            "tracedecay hook(s) missing for {} in {} — run `tracedecay install --local --agent codex` or `tracedecay install --agent codex`",
            missing.join(", "),
            hooks_path.display(),
        ));
    }
}

fn codex_hook_present(hooks: &serde_json::Value, event: &str, command: &str) -> bool {
    hooks["hooks"][event].as_array().is_some_and(|groups| {
        groups.iter().any(|group| {
            group["hooks"].as_array().is_some_and(|handlers| {
                handlers.iter().any(|h| {
                    h["command"]
                        .as_str()
                        .is_some_and(|value| value.contains(command))
                })
            })
        })
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
    }

    #[test]
    fn remove_legacy_codex_native_automation_deletes_stale_record() {
        let home = tempfile::tempdir().expect("tempdir should create");
        assert!(
            !remove_legacy_codex_native_automation(home.path())
                .expect("removal without a record should succeed"),
            "no legacy record should report nothing removed"
        );

        let automation_dir = home
            .path()
            .join(".codex/automations")
            .join(LEGACY_CODEX_NATIVE_AUTOMATION_ID);
        std::fs::create_dir_all(&automation_dir).expect("legacy dir should create");
        std::fs::write(
            automation_dir.join("automation.toml"),
            "status = \"ACTIVE\"\n",
        )
        .expect("legacy automation should write");

        assert!(
            remove_legacy_codex_native_automation(home.path())
                .expect("removal of an existing record should succeed"),
            "an existing legacy record should report removal"
        );
        assert!(
            !automation_dir.exists(),
            "the legacy automation directory should be gone"
        );
    }

    fn relative_paths_under(root: &Path) -> Vec<String> {
        let mut paths: Vec<String> = collect_regular_files(root)
            .expect("plugin source bundle should be readable")
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

    /// The embedded writer is the single source of truth for released installs
    /// (the binary ships without the repo `codex-plugin/` tree), so the list
    /// must cover every file actually present in the source bundle — otherwise
    /// a freshly added skill file would silently never reach Codex users.
    #[test]
    fn codex_embedded_file_list_covers_the_whole_source_bundle() {
        let on_disk = relative_paths_under(&repo_root().join("codex-plugin"));
        let mut expected: Vec<String> = CODEX_EMBEDDED_PLUGIN_FILES
            .iter()
            .map(|&(relative, _)| relative.to_string())
            .collect();
        expected.sort();
        assert_eq!(
            on_disk, expected,
            "CODEX_EMBEDDED_PLUGIN_FILES must cover every codex-plugin file"
        );
    }

    /// Codex auto-discovers skills by description (it has no slash-command or
    /// `disable-model-invocation` surface), so the Codex bundle ships exactly
    /// the *model-invocable* Cursor skills — the same set the Cursor plugin
    /// advertises via [`crate::hooks::CURSOR_PLUGIN_SKILLS`]. The Cursor-only
    /// slash dispatchers (`tracedecay-*`) and explicit-invoke memory skills are
    /// intentionally not mirrored: their workflows are covered by these skills.
    #[test]
    fn codex_bundle_ships_exactly_the_model_invocable_cursor_skills() {
        let mut shipped: Vec<String> = CODEX_EMBEDDED_PLUGIN_FILES
            .iter()
            .filter_map(|&(relative, _)| {
                relative
                    .strip_prefix("skills/")
                    .and_then(|rest| rest.strip_suffix("/SKILL.md"))
                    .map(str::to_string)
            })
            .collect();
        shipped.sort();
        let mut expected: Vec<String> = crate::hooks::CURSOR_PLUGIN_SKILLS
            .iter()
            .map(|skill| (*skill).to_string())
            .collect();
        expected.sort();
        assert_eq!(
            shipped, expected,
            "Codex must embed exactly the model-invocable Cursor skills for parity"
        );
    }

    /// Each Codex skill is a byte-identical mirror of its Cursor source so the
    /// two host plugins never drift. Codex reads only the `name`/`description`
    /// frontmatter for invocation and ignores extra keys, and the skill bodies
    /// reference host-neutral `tracedecay_*` MCP tools, so the same content is
    /// correct in both hosts. Intentional per-skill divergences must be listed
    /// (with a reason) in one of the divergence allowlists below. Per-host
    /// frontmatter schemas (allowed keys per plugin) are enforced separately
    /// by `tests/agent_suite/plugin_skill_contract_test.rs`.
    #[test]
    fn codex_skills_match_the_cursor_source_for_parity() {
        // Skills deliberately specialized for Codex (host-specific bodies that
        // are not compared against the Cursor source at all):
        //
        // - `curating-project-memory`: the Cursor source hands the "add a
        //   researched subject from scratch" flow off to the `memorizing-subject`
        //   skill, an explicit-invoke (`disable-model-invocation: true`) slash
        //   workflow Codex intentionally does not ship. The Codex copy inlines
        //   that flow's guardrails (read-only research, dedupe, cited facts,
        //   secret/PII rejection) instead of pointing at a skill absent here.
        const CODEX_SKILL_BODY_DIVERGENCES: &[&str] = &["curating-project-memory"];
        // Skills whose frontmatter legitimately diverges while the bodies must
        // still mirror byte-for-byte (compared after stripping frontmatter):
        //
        // - `running-impacted-tests`: Cursor keeps `paths` frontmatter so its
        //   host can path-scope the skill, while Codex must omit that key to
        //   satisfy the Codex skill-creator quick_validate.py schema.
        const CODEX_SKILL_FRONTMATTER_DIVERGENCES: &[&str] = &["running-impacted-tests"];
        let root = repo_root();
        for &skill in crate::hooks::CURSOR_PLUGIN_SKILLS {
            let codex_path = root
                .join("codex-plugin/skills")
                .join(skill)
                .join("SKILL.md");
            assert!(
                codex_path.exists(),
                "Codex plugin must ship the `{skill}` skill for parity with Cursor"
            );
            if CODEX_SKILL_BODY_DIVERGENCES.contains(&skill) {
                continue;
            }
            let cursor_body = std::fs::read_to_string(
                root.join("cursor-plugin/skills")
                    .join(skill)
                    .join("SKILL.md"),
            )
            .expect("cursor skill source should be readable");
            let codex_body = std::fs::read_to_string(&codex_path)
                .expect("codex skill source should be readable");
            if CODEX_SKILL_FRONTMATTER_DIVERGENCES.contains(&skill) {
                assert_eq!(
                    lines_after_frontmatter(&codex_body),
                    lines_after_frontmatter(&cursor_body),
                    "Codex `{skill}` skill body must mirror the Cursor source even though \
                     its frontmatter intentionally diverges"
                );
                continue;
            }
            assert_eq!(
                codex_body, cursor_body,
                "Codex `{skill}` skill must mirror the Cursor source (add it to \
                 a CODEX_SKILL_*_DIVERGENCES list if a host-specific version is intended)"
            );
        }
    }

    /// Returns the lines following the closing `---` of the leading YAML
    /// frontmatter. Line-based so CRLF checkouts compare like LF ones.
    fn lines_after_frontmatter(contents: &str) -> Vec<&str> {
        let mut lines = contents.lines();
        assert_eq!(
            lines.next(),
            Some("---"),
            "skill must open YAML frontmatter"
        );
        let mut lines = lines.skip_while(|line| line.trim() != "---");
        assert_eq!(
            lines.next().map(str::trim),
            Some("---"),
            "skill must close YAML frontmatter"
        );
        lines.collect()
    }

    /// Extracts the `<name>` from every `tracedecay:<name>` skill handoff in a
    /// body. MCP tool calls use `tracedecay_*` (underscore) and are ignored.
    fn skill_handoff_references(body: &str) -> Vec<String> {
        const MARKER: &str = "tracedecay:";
        let mut refs = Vec::new();
        let mut rest = body;
        while let Some(pos) = rest.find(MARKER) {
            rest = &rest[pos + MARKER.len()..];
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
                .collect();
            if !name.is_empty() {
                refs.push(name);
            }
        }
        refs
    }

    /// Every `tracedecay:<skill>` handoff inside the embedded Codex skill bodies
    /// must resolve to a skill this bundle actually ships. A dangling reference
    /// (e.g. to a Cursor-only explicit-invoke skill like `memorizing-subject`)
    /// would point a Codex agent at a workflow that does not exist here.
    #[test]
    fn codex_skill_cross_references_resolve_to_shipped_skills() {
        let shipped: std::collections::BTreeSet<String> = CODEX_EMBEDDED_PLUGIN_FILES
            .iter()
            .filter_map(|&(relative, _)| {
                relative
                    .strip_prefix("skills/")
                    .and_then(|rest| rest.strip_suffix("/SKILL.md"))
                    .map(str::to_string)
            })
            .collect();

        let mut dangling: Vec<String> = Vec::new();
        for &(relative, contents) in CODEX_EMBEDDED_PLUGIN_FILES {
            if !relative.starts_with("skills/") {
                continue;
            }
            for reference in skill_handoff_references(contents) {
                if !shipped.contains(&reference) {
                    dangling.push(format!("{relative} -> tracedecay:{reference}"));
                }
            }
        }
        assert!(
            dangling.is_empty(),
            "Codex skill bodies reference skills absent from the bundle: {dangling:?}"
        );
    }
}
