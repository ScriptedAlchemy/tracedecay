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
    safe_write_text_file, write_toml_file, AgentIntegration, DoctorCounters, HealthcheckContext,
    InstallContext, InstallScope, UpdatePluginOutcome,
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
            codex_repo_marketplace_path(project_path),
        ] {
            super::ensure_project_local_safe_path(project_path, &path)?;
        }
        install_codex_repo_plugin(&ctx.home, project_path, &ctx.tracedecay_bin)?;
        sweep_legacy_project_codex_config(project_path);
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
        let legacy_config_install = codex_legacy_config_has_tracedecay(&ctx.home);
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

        // A legacy config-managed install must gain its personal-bundle
        // replacement before the sweep below strips the working global
        // config — even when a cached or repo-local refresh already put
        // something into `refreshed`.
        let has_personal_bundle =
            !cached_dirs.is_empty() || codex_plugin_manifest_path(&ctx.home).exists();
        if refreshed.is_empty() && !has_personal_bundle && !legacy_config_install {
            return Ok(UpdatePluginOutcome::NotInstalled);
        }
        if refreshed.is_empty() || (legacy_config_install && !has_personal_bundle) {
            install_codex_personal_bootstrap(&ctx.home, &ctx.tracedecay_bin)?;
            refreshed.push(plugin_dir.clone());
        }

        if legacy_config_install {
            sweep_legacy_global_codex_config(&ctx.home);
            eprintln!(
                "\x1b[1mAction required:\x1b[0m migrated the legacy Codex config-managed \
                 install to the personal plugin bundle."
            );
            eprintln!("  In Codex, run: codex plugin add tracedecay@personal");
            print_hook_trust_guidance();
        }
        Ok(UpdatePluginOutcome::Refreshed(refreshed))
    }

    fn export_managed_skills(
        &self,
        home: &Path,
        profile_root: &Path,
    ) -> Result<Vec<crate::automation::skill_targets::SkillInstallSummary>> {
        let mut plugin_dirs = codex_plugin_cached_install_dirs(home);
        if codex_plugin_manifest_path(home).exists() {
            plugin_dirs.push(codex_plugin_install_dir(home));
        }
        let mut exports = Vec::new();
        let mut errors = Vec::new();
        for dir in plugin_dirs {
            match crate::automation::skill_targets::install_managed_skills(
                profile_root,
                crate::automation::skill_targets::SkillInstallTarget::Codex,
                &dir,
            ) {
                Ok(summary) => exports.push(summary),
                Err(err) => errors.push(format!("{}: {err}", dir.display())),
            }
        }
        if exports.is_empty() && !errors.is_empty() {
            return Err(TraceDecayError::Config {
                message: errors.join("; "),
            });
        }
        Ok(exports)
    }

    fn export_managed_skills_local(
        &self,
        project_root: &Path,
        profile_root: &Path,
    ) -> Result<Vec<crate::automation::skill_targets::SkillInstallSummary>> {
        let repo_dir = codex_repo_plugin_install_dir(project_root);
        if !repo_dir.join(".codex-plugin/plugin.json").exists()
            || !codex_plugin_dir_is_tracedecay(&repo_dir)
        {
            return Ok(Vec::new());
        }
        Ok(vec![
            crate::automation::skill_targets::install_managed_skills(
                profile_root,
                crate::automation::skill_targets::SkillInstallTarget::Codex,
                &repo_dir,
            )?,
        ])
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mCodex CLI integration\x1b[0m");
        let local_plugin_dir = codex_repo_plugin_install_dir(&ctx.project_path);
        if local_plugin_dir.join(".codex-plugin/plugin.json").exists() {
            doctor_check_plugin_dir(
                dc,
                &local_plugin_dir,
                CodexBundlePolicy::for_scope(InstallScope::ProjectLocal),
                &ctx.home,
            );
            doctor_check_marketplace_entry(
                dc,
                &codex_repo_marketplace_path(&ctx.project_path),
                "repo marketplace",
                "./plugins/tracedecay",
                "tracedecay install --local --agent codex",
            );
            // Repo-local bundles ship no lifecycle hooks by design; hooks come
            // from the personal plugin. Without one, no session/tool hooks run.
            if !codex_plugin_manifest_path(&ctx.home).exists()
                && codex_plugin_cached_install_dirs(&ctx.home).is_empty()
            {
                dc.warn(
                    "repo-local Codex bundles ship no lifecycle hooks — run `tracedecay install --agent codex` to add the personal plugin (session hooks, transcript ingest)",
                );
            }
        } else {
            doctor_check_plugin(dc, &ctx.home);
        }
        doctor_suggest_native_memories_off(dc, &ctx.home);
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
        !codex_plugin_cached_install_dirs(home).is_empty()
            || codex_plugin_manifest_path(home).exists()
    }
}

fn codex_legacy_config_has_tracedecay(home: &Path) -> bool {
    let config = codex_config_path(home);
    if !config.exists() {
        return false;
    }
    super::load_toml_file(&config).is_ok_and(|toml| {
        toml.get("mcp_servers")
            .and_then(|v| v.get("tracedecay"))
            .is_some()
    })
}

// ---------------------------------------------------------------------------
// Install helpers
// ---------------------------------------------------------------------------

/// The Codex plugin's composed deploy set, sourced from the shared `plugin/`
/// tree via [`crate::agents::plugin_bundle::codex_files`]. Each entry is
/// `(deploy_relative_path, file_contents)`; the manifest, `.mcp.json`, and
/// `hooks/hooks.json` entries are rendered at install time to inject the
/// package version and the absolute tracedecay binary path.
fn codex_embedded_plugin_files() -> Vec<(&'static str, &'static str)> {
    crate::agents::plugin_bundle::codex_files()
}

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

/// The user-level Codex config that carries MCP registrations and hook trust.
fn codex_config_path(home: &Path) -> PathBuf {
    home.join(".codex/config.toml")
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
    uninstall_tracedecay_mcp_if_present(&codex_config_path(home));
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

/// The scope contract for a rendered Codex plugin bundle, in one place.
///
/// A global bundle ships lifecycle hooks (declared in the manifest and
/// recorded as trusted in the user-level `~/.codex/config.toml`), serves with
/// the global DB enabled, and carries the memory digest. A repo-local bundle
/// ships no hooks, serves the project path with no env, and stays free of
/// user-profile state. The bundle writer, manifest/MCP renderers, and doctor
/// all consume this type instead of re-encoding the scope as ad-hoc
/// conditionals.
#[derive(Debug, Clone, Copy)]
struct CodexBundlePolicy {
    scope: InstallScope,
}

impl CodexBundlePolicy {
    fn for_scope(scope: InstallScope) -> Self {
        Self { scope }
    }

    /// Whether the bundle ships `hooks/hooks.json` and declares it in the
    /// plugin manifest.
    fn include_hooks(self) -> bool {
        self.scope == InstallScope::Global
    }

    /// The `serve` args baked into the bundle's `.mcp.json`.
    fn mcp_args(self) -> serde_json::Value {
        match self.scope {
            InstallScope::Global => json!(["serve"]),
            InstallScope::ProjectLocal => json!(["serve", "--path", "."]),
        }
    }

    /// The `env` baked into the bundle's `.mcp.json`; `None` strips the key.
    fn mcp_env(self) -> Option<serde_json::Value> {
        match self.scope {
            InstallScope::Global => Some(json!({ "TRACEDECAY_ENABLE_GLOBAL_DB": "1" })),
            InstallScope::ProjectLocal => None,
        }
    }

    /// Where Codex records trust for this bundle's hooks — `None` for scopes
    /// that ship no hooks and therefore have no trust surface.
    fn hook_trust_config_path(self, home: &Path) -> Option<PathBuf> {
        self.include_hooks().then(|| codex_config_path(home))
    }

    /// The memory digest rides only the global bundle.
    fn include_memory_digest(self) -> bool {
        self.scope == InstallScope::Global
    }
}

fn install_codex_plugin_bundle(
    install_dir: &Path,
    tracedecay_bin: &str,
    scope: InstallScope,
    profile_home: &Path,
) -> Result<()> {
    let policy = CodexBundlePolicy::for_scope(scope);
    write_codex_plugin_bundle_base(install_dir, tracedecay_bin, policy)?;
    install_codex_managed_skill_overlay(profile_home, install_dir)?;
    if policy.include_memory_digest() {
        let profile_root =
            crate::automation::skill_targets::profile_root_for_agent_home(profile_home);
        crate::automation::memory_digest::sync_memory_digest_export(
            &profile_root,
            crate::automation::skill_targets::SkillInstallTarget::Codex,
            install_dir,
        )?;
    }
    Ok(())
}

/// Export a complete shareable Codex plugin bundle with active managed skills.
pub fn export_codex_plugin_artifact(
    profile_root: &Path,
    output: &Path,
    tracedecay_bin: &str,
) -> Result<crate::automation::skill_targets::SkillInstallSummary> {
    write_codex_plugin_bundle_base(
        output,
        tracedecay_bin,
        CodexBundlePolicy::for_scope(InstallScope::Global),
    )?;
    crate::automation::skill_targets::export_native_skill_overlay(
        profile_root,
        crate::automation::skill_targets::SkillInstallTarget::Codex,
        output,
    )
}

fn write_codex_plugin_bundle_base(
    install_dir: &Path,
    tracedecay_bin: &str,
    policy: CodexBundlePolicy,
) -> Result<()> {
    if let Some(parent) = install_dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| TraceDecayError::Config {
            message: format!("failed to create {}: {e}", parent.display()),
        })?;
    }
    remove_codex_plugin_install(install_dir)?;
    write_codex_plugin_files(install_dir, tracedecay_bin, policy)
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
    policy: CodexBundlePolicy,
) -> Result<()> {
    for (relative, contents) in codex_embedded_plugin_files() {
        let rendered = match relative {
            ".codex-plugin/plugin.json" => codex_plugin_manifest(contents, policy)?,
            ".mcp.json" => codex_plugin_mcp(contents, tracedecay_bin, policy)?,
            "hooks/hooks.json" if !policy.include_hooks() => continue,
            "hooks/hooks.json" => codex_plugin_hooks(contents, tracedecay_bin)?,
            _ => contents.to_string(),
        };
        safe_write_text_file(&install_dir.join(relative), &rendered, None)?;
    }
    Ok(())
}

fn codex_plugin_manifest(raw: &str, policy: CodexBundlePolicy) -> Result<String> {
    super::plugin_bundle::stamp_manifest_version_with(raw, |manifest| {
        if !policy.include_hooks() {
            if let Some(object) = manifest.as_object_mut() {
                object.remove("hooks");
            }
        }
    })
}

fn codex_plugin_mcp(raw: &str, tracedecay_bin: &str, policy: CodexBundlePolicy) -> Result<String> {
    // Reuse the shared command rewrite, then layer the policy's args/env on
    // top of the result.
    let stamped = super::plugin_bundle::set_mcp_command(raw, tracedecay_bin)?;
    let mut mcp: serde_json::Value = serde_json::from_str(&stamped)?;
    let server = &mut mcp["mcpServers"]["tracedecay"];
    server["args"] = policy.mcp_args();
    match policy.mcp_env() {
        Some(env) => server["env"] = env,
        None => {
            if let Some(object) = server.as_object_mut() {
                object.remove("env");
            }
        }
    }
    Ok(format!("{}\n", serde_json::to_string_pretty(&mcp)?))
}

/// A lifecycle hook the Codex plugin registers.
struct CodexManagedHook {
    event: &'static str,
    subcommand: &'static str,
    timeout_secs: u64,
    matcher: Option<&'static str>,
}

/// Every Codex lifecycle hook, in registration order. The single source of
/// truth for install ([`codex_plugin_hooks`]), uninstall ([`uninstall_hooks`]),
/// and doctor checks ([`doctor_check_hooks`]).
const CODEX_MANAGED_HOOKS: &[CodexManagedHook] = &[
    CodexManagedHook {
        event: "SessionStart",
        subcommand: "hook-codex-session-start",
        timeout_secs: 5,
        matcher: None,
    },
    CodexManagedHook {
        event: "UserPromptSubmit",
        subcommand: "hook-codex-user-prompt-submit",
        timeout_secs: 5,
        matcher: None,
    },
    CodexManagedHook {
        event: "SubagentStart",
        subcommand: "hook-codex-subagent-start",
        timeout_secs: 5,
        matcher: None,
    },
    CodexManagedHook {
        event: "PostToolUse",
        subcommand: "hook-codex-post-tool-use",
        timeout_secs: 60,
        matcher: Some("Bash|apply_patch"),
    },
    CodexManagedHook {
        event: "PostCompact",
        subcommand: "hook-codex-post-compact",
        timeout_secs: 120,
        matcher: Some("auto|manual"),
    },
];

/// Subcommands from older bundles that uninstall must also strip even though
/// the current bundle no longer registers them.
const CODEX_LEGACY_HOOK_SUBCOMMANDS: &[&str] = &["hook-codex-pre-tool-use"];
const CODEX_PERSONAL_PLUGIN_HOOK_TRUST_PREFIX: &str = "tracedecay@personal:hooks/hooks.json:";

#[derive(Debug, PartialEq, Eq)]
enum CodexHookTrustState {
    Trusted,
    Missing(Vec<String>),
}

/// Codex records hook state under `snake_case` event keys. Derive them from the
/// managed hook's subcommand (`hook-codex-post-tool-use` -> `post_tool_use`)
/// so the mapping stays anchored to the single-source-of-truth table instead
/// of re-implementing Codex's name normalization.
fn codex_hook_state_event_key(hook: &CodexManagedHook) -> String {
    hook.subcommand
        .trim_start_matches("hook-codex-")
        .replace('-', "_")
}

fn codex_plugin_hook_trust_state(config: &toml::Value) -> CodexHookTrustState {
    // A missing [hooks.state] table is just "nothing trusted yet" — treat it
    // as empty so one pipeline produces the missing list either way.
    let empty = toml::value::Table::new();
    let state = config
        .get("hooks")
        .and_then(|hooks| hooks.get("state"))
        .and_then(|state| state.as_table())
        .unwrap_or(&empty);

    let missing: Vec<String> = CODEX_MANAGED_HOOKS
        .iter()
        .map(codex_hook_state_event_key)
        .filter(|event_key| {
            let trust_key = format!("{CODEX_PERSONAL_PLUGIN_HOOK_TRUST_PREFIX}{event_key}:0:0");
            !state.get(&trust_key).is_some_and(|entry| {
                entry
                    .get("trusted_hash")
                    .and_then(|hash| hash.as_str())
                    .is_some_and(|hash| hash.starts_with("sha256:"))
            })
        })
        .collect();

    if missing.is_empty() {
        CodexHookTrustState::Trusted
    } else {
        CodexHookTrustState::Missing(missing)
    }
}

fn codex_plugin_hooks(raw: &str, tracedecay_bin: &str) -> Result<String> {
    let mut hooks: serde_json::Value = serde_json::from_str(raw)?;
    for hook in CODEX_MANAGED_HOOKS {
        install_codex_hook_event(
            &mut hooks,
            hook.event,
            tracedecay_bin,
            hook.subcommand,
            hook.timeout_secs,
            hook.matcher,
        );
    }
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
    let profile_root = crate::automation::skill_targets::profile_root_for_agent_home(home);
    for install_dir in codex_plugin_cached_install_dirs(home) {
        crate::automation::memory_digest::remove_memory_digest_export(
            &profile_root,
            crate::automation::skill_targets::SkillInstallTarget::Codex,
            &install_dir,
        )?;
        remove_codex_plugin_bootstrap_source(&install_dir)?;
    }
    let install_dir = codex_plugin_install_dir(home);
    crate::automation::memory_digest::remove_memory_digest_export(
        &profile_root,
        crate::automation::skill_targets::SkillInstallTarget::Codex,
        &install_dir,
    )?;
    remove_codex_plugin_bootstrap_source(&install_dir)?;
    remove_codex_marketplace_entry(home)?;
    Ok(())
}

fn uninstall_codex_repo_plugin_if_present(ctx: &InstallContext) -> Result<()> {
    let Some(project_path) = codex_update_project_path(ctx) else {
        return Ok(());
    };
    let install_dir = codex_repo_plugin_install_dir(&project_path);
    let profile_root = crate::automation::skill_targets::profile_root_for_agent_home(&ctx.home);
    crate::automation::memory_digest::remove_memory_digest_export(
        &profile_root,
        crate::automation::skill_targets::SkillInstallTarget::Codex,
        &install_dir,
    )?;
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
    sweep_retired_bundle_skill_dirs(skills_dir);
    let managed: HashSet<PathBuf> = codex_plugin_managed_paths(install_dir)
        .into_iter()
        .filter(|path| path.starts_with(skills_dir))
        .collect();
    let mut files =
        super::collect_regular_files(skills_dir).map_err(|e| TraceDecayError::Config {
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

/// Remove every `skills/<dir>` under the Codex plugin dir that the current
/// bundle no longer ships. The keep-set is derived from the live embedded
/// bundle (plus the agent-managed overlays deployed separately), so any retired
/// skill is swept on upgrade without a hand-maintained legacy list.
///
/// Only tracedecay-owned skill dirs are swept: a same-name user-authored skill
/// whose `SKILL.md` carries no tracedecay marker is left untouched, so an
/// upgrade never deletes a user's private workflow that collides with a retired
/// bundle slug.
fn sweep_retired_bundle_skill_dirs(skills_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(skills_dir) else {
        return;
    };
    let mut shipped: std::collections::BTreeSet<String> = codex_embedded_plugin_files()
        .into_iter()
        .filter_map(|(relative, _)| {
            relative
                .strip_prefix("skills/")
                .and_then(|rest| rest.split('/').next())
                .map(str::to_string)
        })
        .collect();
    // The agent-managed overlays are deployed/removed separately; never treat
    // them as retired.
    shipped.insert("agent-managed".to_string());
    shipped.insert("agent-managed-memory".to_string());
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if shipped.contains(&name) {
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

/// True when a Codex `SKILL.md` at `skill_file` carries a tracedecay authorship
/// marker, marking the skill dir as tracedecay-owned.
fn skill_file_has_tracedecay_marker(skill_file: &Path) -> bool {
    std::fs::read_to_string(skill_file)
        .is_ok_and(|contents| super::skill_contents_have_tracedecay_marker(&contents))
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
    let Ok(entries) = super::collect_regular_files(install_dir) else {
        return false;
    };
    let managed = codex_plugin_managed_paths(install_dir);
    entries.iter().all(|entry| managed.contains(entry))
}

fn codex_plugin_managed_paths(install_dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = codex_embedded_plugin_files()
        .into_iter()
        .map(|(relative, _)| install_dir.join(relative))
        .collect();
    paths.push(install_dir.join("skills/agent-managed-memory/SKILL.md"));
    paths
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
    let subcommands: Vec<&str> = CODEX_MANAGED_HOOKS
        .iter()
        .map(|hook| hook.subcommand)
        .chain(CODEX_LEGACY_HOOK_SUBCOMMANDS.iter().copied())
        .collect();

    if !hooks_path.exists() {
        return;
    }
    let Ok(mut hooks) = load_json_file_strict(hooks_path) else {
        return;
    };

    let Some(events) = hooks.get_mut("hooks").and_then(|h| h.as_object_mut()) else {
        return;
    };
    let mut removed_any = false;
    for groups in events.values_mut() {
        if let Some(arr) = groups.as_array_mut() {
            let before = arr.len();
            arr.retain(|group| !subcommands.iter().any(|sc| group_has_subcommand(group, sc)));
            removed_any |= arr.len() != before;
        }
    }
    if !removed_any {
        return;
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
        let _ = super::backup_file(config_path);
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
    let global_policy = CodexBundlePolicy::for_scope(InstallScope::Global);
    let cached_dirs = codex_plugin_cached_install_dirs(home);
    if !cached_dirs.is_empty() {
        for plugin_dir in cached_dirs {
            doctor_check_plugin_dir(dc, &plugin_dir, global_policy, home);
        }
        return;
    }

    let plugin_dir = codex_plugin_install_dir(home);
    let manifest_path = plugin_dir.join(".codex-plugin/plugin.json");
    if !manifest_path.exists() {
        dc.warn(&format!(
            "{} not found — run `tracedecay install --agent codex` or `tracedecay update-plugin` to install the Codex plugin bundle",
            manifest_path.display()
        ));
        return;
    }

    doctor_check_plugin_dir(dc, &plugin_dir, global_policy, home);
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

fn doctor_check_plugin_dir(
    dc: &mut DoctorCounters,
    plugin_dir: &Path,
    policy: CodexBundlePolicy,
    home: &Path,
) {
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
    let hooks_path = plugin_dir.join("hooks/hooks.json");
    if let Some(config_path) = policy.hook_trust_config_path(home) {
        doctor_check_hooks(dc, &hooks_path, &config_path);
    } else if hooks_path.exists() {
        dc.warn(&format!(
            "repo-local Codex bundle unexpectedly ships lifecycle hooks in {} — run `tracedecay install --local --agent codex` to refresh it",
            hooks_path.display()
        ));
    }
}

/// Check hooks.json registers the tracedecay lifecycle hooks, and report Codex
/// hook trust state from the user-level config.
fn doctor_check_hooks(dc: &mut DoctorCounters, hooks_path: &Path, config_path: &Path) {
    if !hooks_path.exists() {
        dc.warn(&format!(
            "{} not found — run `tracedecay install --agent codex` to add lifecycle hooks",
            hooks_path.display()
        ));
        return;
    }
    let hooks = super::load_json_file(hooks_path);
    let missing: Vec<&str> = CODEX_MANAGED_HOOKS
        .iter()
        .filter_map(|hook| {
            (!codex_hook_present(&hooks, hook.event, hook.subcommand)).then_some(hook.event)
        })
        .collect();
    if !missing.is_empty() {
        dc.warn(&format!(
            "tracedecay hook(s) missing for {} in {} — run `tracedecay install --agent codex`",
            missing.join(", "),
            hooks_path.display(),
        ));
        return;
    }

    dc.pass(&format!(
        "All {} Codex lifecycle hooks registered in {}",
        CODEX_MANAGED_HOOKS.len(),
        hooks_path.display()
    ));
    match load_toml_file(config_path) {
        Ok(config) => match codex_plugin_hook_trust_state(&config) {
            CodexHookTrustState::Trusted => dc.info(&format!(
                "Codex hook trust entries recorded in {} — trust is pinned to hook content, so if hooks changed since trusting (e.g. after update-plugin), run /hooks in Codex to re-trust",
                config_path.display()
            )),
            CodexHookTrustState::Missing(missing) => dc.info(&format!(
                "Codex skips new/changed command hooks until trusted — missing trust for {} in {}; run `/hooks` in Codex",
                missing.join(", "),
                config_path.display()
            )),
        },
        Err(_) => dc.info(
            "Codex skips new/changed command hooks until trusted — run `/hooks` in Codex to trust the tracedecay hooks",
        ),
    }
}

/// Suggests turning off Codex's native memories *injection* when tracedecay's
/// fact-store injection is active, so the model does not receive two parallel
/// memory systems built from the same sessions. This is advisory only: the
/// user's `config.toml` is never edited, and tracedecay never writes into
/// `~/.codex/memories/` — the holographic fact store stays the single source
/// of truth and delivery is rendered prompt context only.
fn doctor_suggest_native_memories_off(dc: &mut DoctorCounters, home: &Path) {
    if !crate::hooks::memory_inject::memory_injection_enabled() {
        return;
    }
    let config_path = codex_config_path(home);
    let Ok(config) = load_toml_file(&config_path) else {
        return;
    };
    if codex_native_memories_injection_enabled(&config) {
        dc.info(
            "Codex native memories injection is enabled alongside tracedecay's \
             fact-store injection; consider setting `memories.use_memories = false` \
             in ~/.codex/config.toml so per-project memory comes from the tracedecay \
             fact store only (tracedecay never edits this setting itself)",
        );
    }
}

/// True when Codex's experimental memories feature is on and session-start
/// memory injection (`memories.use_memories`, default true) is not disabled.
fn codex_native_memories_injection_enabled(config: &toml::Value) -> bool {
    let memories_feature_on = config
        .get("features")
        .and_then(|features| features.get("memories"))
        .is_some_and(|memories| {
            // `memories = true` (bool) or the nested `[features.memories]` table
            // form both mean the feature is enabled.
            memories.as_bool().unwrap_or(memories.is_table())
        });
    if !memories_feature_on {
        return false;
    }
    config
        .get("memories")
        .and_then(|memories| memories.get("use_memories"))
        .and_then(toml::Value::as_bool)
        .unwrap_or(true)
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
mod tests;
