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
use toml_edit::{DocumentMut, Item, Table, value};
use tracedecay_domain::canonical_sha256;

use crate::errors::{Result, TraceDecayError};

use super::{
    AgentIntegration, DeferredUserAction, DoctorCounters, HealthcheckContext, InstallContext,
    InstallScope, NonInteractiveInstallOutcome, UpdatePluginOutcome, config_backup_path,
    load_json_file, load_json_file_strict, load_toml_file, safe_write_json_file,
    safe_write_text_file,
};

/// Codex records an activated plugin as `[plugins."<plugin>@<marketplace>"]
/// enabled = true` in `~/.codex/config.toml`, alongside a materialised bundle
/// under `~/.codex/plugins/cache/<marketplace>/<plugin>/<version>`. Both facts
/// are plain files TraceDecay already owns writers for, so activation runs
/// non-interactively; `codex plugin add <plugin>@<marketplace>` writes exactly
/// the same pair and stays the operator-facing fallback.
///
/// The prefix every activation key for this plugin starts with, whatever
/// marketplace it was installed from.
const CODEX_PLUGIN_ACTIVATION_KEY_PREFIX: &str = "tracedecay@";

/// Exact one-time step an operator runs when TraceDecay refused to touch a
/// Codex config whose `[plugins]` shape it does not recognise.
fn codex_manual_activation_step(marketplace_name: &str) -> String {
    format!("codex plugin add tracedecay@{marketplace_name}")
}

/// Operator-facing remediation for the fail-safe path: TraceDecay found a
/// `config.toml` it will not rewrite, so it names the exact command that
/// finishes activation instead.
fn codex_deferred_activation_guidance(home: &Path, reason: &str) -> String {
    format!(
        "Codex plugin activation was left to you: {reason}. Run `{}`, then re-run doctor.",
        codex_manual_activation_step(&codex_cached_marketplace_name(home))
    )
}

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
        let deferred_marketplace = announce_codex_plugin_activation(&ctx.home, &ctx.tracedecay_bin);

        eprintln!();
        eprintln!("Setup complete. Next steps:");
        eprintln!("  1. cd into your project and run: tracedecay init");
        match deferred_marketplace {
            None => {
                eprintln!("  2. Start a new Codex session — tracedecay tools are now available");
            }
            Some(marketplace_name) => {
                eprintln!(
                    "  2. Run: {}",
                    codex_manual_activation_step(&marketplace_name)
                );
                eprintln!("  3. Start a new Codex session — tracedecay tools are now available");
            }
        }
        announce_codex_hook_trust(&ctx.home, &ctx.tracedecay_bin);
        Ok(())
    }

    fn supports_local_install(&self) -> bool {
        true
    }

    fn preflight_non_interactive_install(
        &self,
        ctx: &InstallContext,
    ) -> Result<NonInteractiveInstallOutcome> {
        // Codex activation is two file writes TraceDecay already owns (see
        // `codex_activate_plugin`), so the ordinary install path converges it.
        // The only deferral left is the fail-safe: a `config.toml` whose
        // `[plugins]` shape TraceDecay refuses to rewrite.
        match codex_unwritable_activation_reason(&ctx.home) {
            None => Ok(NonInteractiveInstallOutcome::Ready),
            Some(reason) => Ok(NonInteractiveInstallOutcome::DeferredUserAction(
                DeferredUserAction {
                    remediation: codex_deferred_activation_guidance(&ctx.home, &reason),
                    staged_paths: Vec::new(),
                },
            )),
        }
    }

    fn interactive_activation_guidance(&self) -> Option<String> {
        // Codex has a supported non-interactive activation surface, so doctor
        // must keep the blocking classification for absent artifacts: an
        // unattended reinstall really does converge them.
        None
    }

    fn prepare_non_interactive_install(
        &self,
        ctx: &InstallContext,
    ) -> Result<NonInteractiveInstallOutcome> {
        install_codex_plugin(&ctx.home, &ctx.tracedecay_bin)?;
        // Activating here is what lets the receipt-backed lifecycle observe a
        // `Current` registration for an install it is about to record. Hook
        // trust rides along because that lifecycle then has no activation step
        // left to apply; a failure to record it only costs a `/hooks` prompt,
        // so it stays advisory.
        match codex_activate_plugin(&ctx.home, &ctx.tracedecay_bin) {
            Ok(_) => {
                announce_codex_hook_trust(&ctx.home, &ctx.tracedecay_bin);
                Ok(NonInteractiveInstallOutcome::Ready)
            }
            Err(error) => Ok(NonInteractiveInstallOutcome::DeferredUserAction(
                DeferredUserAction {
                    remediation: codex_deferred_activation_guidance(&ctx.home, &error.to_string()),
                    staged_paths: vec![
                        codex_plugin_manifest_path(&ctx.home),
                        codex_personal_marketplace_path(&ctx.home),
                    ],
                },
            )),
        }
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
        install_codex_managed_agents(&ctx.home)?;
        sweep_legacy_project_codex_config(project_path);
        Ok(())
    }

    fn uninstall_local(&self, ctx: &InstallContext, project_path: &Path) -> Result<()> {
        let local = InstallContext {
            home: ctx.home.clone(),
            tracedecay_bin: ctx.tracedecay_bin.clone(),
            tool_permissions: ctx.tool_permissions.clone(),
            project_root: Some(project_path.to_path_buf()),
            dashboard: ctx.dashboard,
        };
        uninstall_codex_repo_plugin_if_present(&local)
    }

    fn uninstall(&self, ctx: &InstallContext) -> Result<()> {
        let codex_dir = ctx.home.join(".codex");
        let config_path = codex_dir.join("config.toml");
        uninstall_codex_config(&config_path)?;
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
        let legacy_config_install = codex_legacy_config_has_tracedecay_for_update(&ctx.home);
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
        let personal_bundle_exists = codex_plugin_manifest_path(&ctx.home).exists();
        let has_personal_bundle = !cached_dirs.is_empty() || personal_bundle_exists;
        if refreshed.is_empty() && !has_personal_bundle && !legacy_config_install {
            return Ok(UpdatePluginOutcome::NotInstalled);
        }
        install_codex_managed_agents(&ctx.home)?;
        if (cached_dirs.is_empty() && personal_bundle_exists)
            || refreshed.is_empty()
            || (legacy_config_install && !has_personal_bundle)
        {
            install_codex_personal_bootstrap(&ctx.home, &ctx.tracedecay_bin)?;
            refreshed.push(plugin_dir.clone());
        }

        if legacy_config_install {
            sweep_legacy_global_codex_config(&ctx.home);
            eprintln!(
                "\x1b[1mMigrated:\x1b[0m the legacy Codex config-managed install is now the \
                 personal plugin bundle."
            );
        }
        // Re-activate and auto-trust the personal bundle's hooks whenever one is
        // present, so a refresh (which may have changed hook content or the
        // cached version directory) re-pins both without a manual /hooks
        // approval or plugin-UI visit. Repo-local-only installs ship no hooks
        // and have no personal activation surface, so this is a no-op for them.
        if codex_plugin_manifest_path(&ctx.home).exists()
            || !codex_plugin_cached_install_dirs(&ctx.home).is_empty()
        {
            if let Some(marketplace_name) =
                announce_codex_plugin_activation(&ctx.home, &ctx.tracedecay_bin)
            {
                eprintln!(
                    "\x1b[1mAction required:\x1b[0m run {}",
                    codex_manual_activation_step(&marketplace_name)
                );
            }
            announce_codex_hook_trust(&ctx.home, &ctx.tracedecay_bin);
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

    fn host_component_registration(
        &self,
        _component: super::host_bundle_v2::HostBundleComponentV1,
        ctx: &HealthcheckContext,
    ) -> super::host_bundle_v2::HostBundleRegistrationStateV1 {
        use super::host_bundle_v2::HostBundleRegistrationStateV1 as State;

        // Registration state is decided by the registration surface alone. The
        // deployed plugin *source* bundle is an artifact-layer deployment that
        // `deactivate_deployed_host_registration` deliberately leaves behind for
        // the artifact layer to remove, so it must not keep this reporting a
        // non-`Missing` state after deactivation — the uninstall verify demands
        // `Missing` and would otherwise roll a correct uninstall back.
        match codex_registration_residue(&ctx.home) {
            Ok(false) => return State::Missing,
            Ok(true) => {}
            Err(()) => return State::Corrupt,
        }

        let candidates = [
            ctx.home.join(".codex/plugins/tracedecay"),
            codex_plugin_install_dir(&ctx.home),
        ];
        let Some(plugin_dir) = candidates
            .into_iter()
            .find(|path| path.join(".codex-plugin/plugin.json").is_file())
        else {
            return State::Missing;
        };
        let manifest = load_json_file(&plugin_dir.join(".codex-plugin/plugin.json"));
        if manifest.get("name").and_then(serde_json::Value::as_str) != Some("tracedecay") {
            return State::Corrupt;
        }
        // A deployed source bundle alone is not activation. Codex's own
        // readback for "this plugin is installed and enabled" is the pair
        // TraceDecay writes in `codex_activate_plugin`: a materialised cache
        // bundle plus `enabled = true` in `config.toml`.
        match codex_plugin_activation_state(&ctx.home) {
            Ok(true) => State::Current,
            Ok(false) => State::Repairable,
            Err(()) => State::Corrupt,
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

    fn host_registration_paths(&self, home: &Path) -> Vec<PathBuf> {
        let mut paths = vec![
            codex_config_path(home),
            codex_personal_marketplace_path(home),
        ];
        paths.extend([
            config_backup_path(&codex_config_path(home)),
            config_backup_path(&codex_personal_marketplace_path(home)),
        ]);
        let current_cache = codex_plugin_current_cached_install_dir(home);
        paths.extend(codex_plugin_managed_paths(&current_cache));
        for cache in codex_plugin_cached_install_dirs(home) {
            paths.extend(codex_plugin_managed_paths(&cache));
        }
        paths.sort();
        paths.dedup();
        paths
    }

    fn host_component_registration_paths(
        &self,
        components: &[super::host_bundle_v2::HostBundleComponentV1],
        home: &Path,
    ) -> Vec<PathBuf> {
        let mut paths = self.host_registration_paths(home);
        if components.contains(&super::host_bundle_v2::HostBundleComponentV1::Core) {
            paths.extend(crate::automation::agent_targets::managed_agent_transaction_paths(home));
        }
        paths
    }

    fn activate_deployed_host_registration(&self, ctx: &InstallContext) -> Result<()> {
        // `~/.codex/agents` is registration surface, not deployed component
        // assets: `host_component_registration_paths` declares every generated
        // export plus the ownership manifest for Core. The legacy `install`
        // path refreshed them through `install_codex_plugin`, so activation
        // has to do it here too — otherwise a Core install through the
        // receipt-backed lifecycle never writes the current exports and never
        // retires the ones a previous bundle owned.
        install_codex_managed_agents(&ctx.home)?;
        codex_activate_plugin(&ctx.home, &ctx.tracedecay_bin)?;
        sync_codex_hook_trust(&ctx.home, &ctx.tracedecay_bin)?;
        Ok(())
    }

    fn deactivate_deployed_host_registration(&self, ctx: &InstallContext) -> Result<()> {
        crate::automation::agent_targets::remove_managed_agents(&ctx.home.join(".codex/agents"))?;
        uninstall_codex_config(&codex_config_path(&ctx.home))?;
        remove_codex_marketplace_entry(&ctx.home)
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        !codex_plugin_cached_install_dirs(home).is_empty()
            || codex_plugin_manifest_path(home).exists()
    }
}

fn codex_legacy_config_has_tracedecay(home: &Path) -> Result<bool> {
    codex_config_has_tracedecay_mcp_server(&codex_config_path(home))
}

fn codex_legacy_config_has_tracedecay_for_update(home: &Path) -> bool {
    match codex_legacy_config_has_tracedecay(home) {
        Ok(has_tracedecay) => has_tracedecay,
        Err(err) => {
            eprintln!(
                "  Could not inspect legacy Codex MCP config at {}: {err}",
                codex_config_path(home).display()
            );
            false
        }
    }
}

fn codex_config_has_tracedecay_mcp_server(config_path: &Path) -> Result<bool> {
    if !config_path.exists() {
        return Ok(false);
    }
    let toml = super::load_toml_file(config_path)?;
    Ok(toml
        .get("mcp_servers")
        .and_then(|v| v.get("tracedecay"))
        .is_some())
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

fn codex_plugin_cached_root(home: &Path, marketplace_name: &str) -> PathBuf {
    home.join(".codex/plugins/cache")
        .join(marketplace_name)
        .join("tracedecay")
}

fn validate_codex_marketplace_name(name: &str) -> Result<&str> {
    crate::storage::validate_project_id(name).map_err(|_| TraceDecayError::Config {
        message: format!("Codex marketplace name {name:?} must be a safe ASCII path segment"),
    })?;
    Ok(name)
}

fn codex_cached_marketplace_name(home: &Path) -> String {
    match codex_personal_marketplace_name(home).as_deref() {
        Ok("caveman-home") | Err(_) => CODEX_DEFAULT_MARKETPLACE_NAME.to_string(),
        Ok(name) => name.to_string(),
    }
}

fn codex_plugin_current_cached_install_dir(home: &Path) -> PathBuf {
    codex_plugin_cached_root(home, &codex_cached_marketplace_name(home))
        .join(crate::PRODUCT_VERSION)
}

fn codex_plugin_cached_install_dirs(home: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut marketplace_names = vec![
        codex_cached_marketplace_name(home),
        CODEX_DEFAULT_MARKETPLACE_NAME.to_string(),
        "caveman-home".to_string(),
    ];
    marketplace_names.sort();
    marketplace_names.dedup();
    for marketplace_name in marketplace_names {
        let root = codex_plugin_cached_root(home, &marketplace_name);
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        dirs.extend(
            entries
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| path.is_dir() && codex_plugin_dir_is_tracedecay(path)),
        );
    }
    dirs.sort();
    dirs.dedup();
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
    install_codex_managed_agents(home)?;
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
    match codex_config_has_tracedecay_mcp_server(config_path) {
        Ok(true) => {}
        Ok(false) => return,
        Err(err) => {
            eprintln!(
                "  Could not inspect project-local Codex MCP config at {}: {err}",
                config_path.display()
            );
            return;
        }
    }
    if let Err(err) = uninstall_codex_config(config_path) {
        eprintln!(
            "  Could not remove project-local Codex MCP config from {}: {err}",
            config_path.display()
        );
    }
}

/// The scope contract for a rendered Codex plugin bundle, in one place.
///
/// A global bundle ships lifecycle hooks (declared in the manifest and
/// recorded as trusted in the user-level `~/.codex/config.toml`), invokes
/// `serve` without an explicit project path, and carries the memory digest. A
/// repo-local bundle ships no hooks, invokes `serve --path .` with no env, and
/// stays free of user-profile state. The bundle writer, manifest/MCP renderers,
/// and doctor all consume this type instead of re-encoding the scope as ad-hoc
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

fn install_codex_managed_agents(
    home: &Path,
) -> Result<crate::automation::agent_targets::ManagedAgentInstallSummary> {
    crate::automation::agent_targets::install_codex_managed_agents(home)
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
    for (relative, rendered) in rendered_plugin_files(tracedecay_bin, policy)? {
        safe_write_text_file(&install_dir.join(relative), &rendered, None)?;
    }
    Ok(())
}

/// Canonical rendered global Codex plugin inventory. The registration probe
/// inspects `.codex/plugins/tracedecay` — the directory the receipt-backed
/// first-party host-bundle catalog owns — and requires the managed lifecycle
/// hooks to be present, so the catalog must deploy the same rendered content
/// the installer produces instead of the raw templates (whose `hooks.json`
/// is an empty scaffold rendered only at install time).
pub(crate) fn rendered_global_plugin_files(
    tracedecay_bin: &str,
) -> Result<Vec<(&'static str, String)>> {
    rendered_plugin_files(
        tracedecay_bin,
        CodexBundlePolicy::for_scope(InstallScope::Global),
    )
}

fn rendered_plugin_files(
    tracedecay_bin: &str,
    policy: CodexBundlePolicy,
) -> Result<Vec<(&'static str, String)>> {
    codex_embedded_plugin_files()
        .into_iter()
        .filter_map(|(relative, contents)| {
            let rendered = match relative {
                ".codex-plugin/plugin.json" => codex_plugin_manifest(contents, policy),
                ".mcp.json" => codex_plugin_mcp(contents, tracedecay_bin, policy),
                "hooks/hooks.json" if !policy.include_hooks() => return None,
                "hooks/hooks.json" => codex_plugin_hooks(contents, tracedecay_bin),
                _ => Ok(contents.to_string()),
            };
            Some(rendered.map(|rendered| (relative, rendered)))
        })
        .collect()
}

fn codex_plugin_manifest(raw: &str, policy: CodexBundlePolicy) -> Result<String> {
    super::plugin_bundle::stamp_manifest_version_with(raw, |manifest| {
        if !policy.include_hooks()
            && let Some(object) = manifest.as_object_mut()
        {
            object.remove("hooks");
        }
    })
}

fn codex_plugin_mcp(raw: &str, tracedecay_bin: &str, policy: CodexBundlePolicy) -> Result<String> {
    // Reuse the shared command rewrite, then layer the policy's args/env on
    // top of the result.
    let stamped = super::plugin_bundle::set_mcp_command(raw, tracedecay_bin)?;
    let mut mcp: serde_json::Value = serde_json::from_str(&stamped)?;
    let server = &mut mcp["mcpServers"]["graph"];
    server["args"] = policy.mcp_args();
    server["startup_timeout_sec"] = CODEX_MCP_STARTUP_TIMEOUT_SECS.into();
    server["tool_timeout_sec"] = CODEX_MCP_TOOL_TIMEOUT_SECS.into();
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
    CodexManagedHook {
        event: "Stop",
        subcommand: "hook-codex-stop",
        timeout_secs: 5,
        matcher: None,
    },
];

/// Subcommands from older bundles that uninstall must also strip even though
/// the current bundle no longer registers them.
const CODEX_LEGACY_HOOK_SUBCOMMANDS: &[&str] = &["hook-codex-pre-tool-use"];
const CODEX_DEFAULT_MARKETPLACE_NAME: &str = "personal";
const CODEX_MCP_STARTUP_TIMEOUT_SECS: u64 = 120;
const CODEX_MCP_TOOL_TIMEOUT_SECS: u64 = 900;

fn codex_mcp_timeouts_current(mcp: &serde_json::Value) -> bool {
    let Some(server) = mcp.pointer("/mcpServers/graph") else {
        return false;
    };
    server
        .get("startup_timeout_sec")
        .and_then(serde_json::Value::as_u64)
        == Some(CODEX_MCP_STARTUP_TIMEOUT_SECS)
        && server
            .get("tool_timeout_sec")
            .and_then(serde_json::Value::as_u64)
            == Some(CODEX_MCP_TOOL_TIMEOUT_SECS)
}

#[derive(Debug, PartialEq, Eq)]
enum CodexHookTrustState {
    Trusted,
    /// Trust entries for these event labels are absent from `config.toml`.
    Missing(Vec<String>),
    /// Trust entries exist for these event labels but their stored hash no
    /// longer matches the current `hooks.json` content (e.g. after a bundle
    /// upgrade changed a command or timeout). Codex silently skips such hooks.
    Modified(Vec<String>),
}

/// A single trust record Codex expects in `~/.codex/config.toml` for one
/// tracedecay-personal-plugin command hook handler.
///
/// The `trust_key` is the fully-qualified `[hooks.state."…"]` table name and
/// `hash` is the `sha256:<hex>` content hash Codex records as `trusted_hash`.
/// `command` is retained so the installer's safety valve can confirm the hook
/// actually invokes the tracedecay binary before recording trust for it.
#[derive(Debug, Clone)]
struct CodexHookTrustEntry {
    event_label: String,
    trust_key: String,
    hash: String,
    command: String,
}

/// Convert a Codex `CamelCase` lifecycle event name to the `snake_case` label
/// Codex uses in trust keys and in the hook hash identity
/// (`PostToolUse` -> `post_tool_use`). This mirrors Codex's own normalization
/// so entries computed here match the TUI's `/hooks` approval byte-for-byte.
fn codex_event_snake_case(event: &str) -> String {
    let mut out = String::new();
    for (index, ch) in event.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// Compute Codex's `trusted_hash` for one command-hook handler identity.
///
/// This is a direct port of Codex's `command_hook_hash`: build the handler
/// identity object (`event_name`, optional `matcher`, and a single-element
/// `hooks` array carrying the `command`/`timeout`/`async` handler), canonicalize
/// it (recursively key-sorted, compact JSON), sha256 the bytes, and format
/// `sha256:<lowercase hex>`. `async` and `timeout` are always present after
/// normalization; `matcher` is omitted entirely when the group has none.
fn codex_command_hook_hash(
    event_name: &str,
    matcher: Option<&str>,
    command: &str,
    timeout: u64,
    is_async: bool,
) -> Result<String> {
    codex_command_hook_hash_with(
        event_name,
        matcher,
        command,
        timeout,
        is_async,
        |identity| {
            canonical_sha256(identity)
                .map(|digest| digest.to_string())
                .map_err(|error| error.to_string())
        },
    )
}

fn codex_command_hook_hash_with(
    event_name: &str,
    matcher: Option<&str>,
    command: &str,
    timeout: u64,
    is_async: bool,
    canonicalize: impl FnOnce(&serde_json::Value) -> std::result::Result<String, String>,
) -> Result<String> {
    let handler = json!({
        "type": "command",
        "command": command,
        "timeout": timeout,
        "async": is_async,
    });
    let mut identity = serde_json::Map::new();
    identity.insert("event_name".to_string(), json!(event_name));
    if let Some(matcher) = matcher {
        identity.insert("matcher".to_string(), json!(matcher));
    }
    identity.insert("hooks".to_string(), json!([handler]));
    canonicalize(&serde_json::Value::Object(identity)).map_err(|error| TraceDecayError::Config {
        message: format!("failed to canonicalize Codex hook trust identity: {error}"),
    })
}

/// Derive the ordered trust records for a rendered Codex `hooks.json` value.
///
/// Iterates events -> groups -> handlers exactly as Codex indexes them, so the
/// group/handler positions in each `trust_key` match what Codex records. The
/// per-handler `timeout` is normalized the way Codex does (default 600, clamped
/// to a minimum of 1) and `async` defaults to false, so the hash matches the
/// TUI's `/hooks` approval regardless of whether those keys are present on disk.
fn codex_plugin_hook_trust_prefix(marketplace_name: &str) -> String {
    format!("tracedecay@{marketplace_name}:hooks/hooks.json:")
}

#[cfg(test)]
fn codex_hook_trust_entries(hooks: &serde_json::Value) -> Result<Vec<CodexHookTrustEntry>> {
    codex_hook_trust_entries_for_marketplace(hooks, CODEX_DEFAULT_MARKETPLACE_NAME)
}

fn codex_hook_trust_entries_for_marketplace(
    hooks: &serde_json::Value,
    marketplace_name: &str,
) -> Result<Vec<CodexHookTrustEntry>> {
    let mut entries = Vec::new();
    let trust_prefix = codex_plugin_hook_trust_prefix(marketplace_name);
    let Some(events) = hooks.get("hooks").and_then(|hooks| hooks.as_object()) else {
        return Ok(entries);
    };
    for (event_key, groups) in events {
        let event_label = codex_event_snake_case(event_key);
        let Some(groups) = groups.as_array() else {
            continue;
        };
        for (group_index, group) in groups.iter().enumerate() {
            let matcher = group.get("matcher").and_then(|value| value.as_str());
            let Some(handlers) = group.get("hooks").and_then(|value| value.as_array()) else {
                continue;
            };
            for (handler_index, handler) in handlers.iter().enumerate() {
                let Some(command) = handler.get("command").and_then(|value| value.as_str()) else {
                    continue;
                };
                let timeout = handler
                    .get("timeout")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(600)
                    .max(1);
                let is_async = handler
                    .get("async")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let hash =
                    codex_command_hook_hash(&event_label, matcher, command, timeout, is_async)?;
                let trust_key =
                    format!("{trust_prefix}{event_label}:{group_index}:{handler_index}");
                entries.push(CodexHookTrustEntry {
                    event_label: event_label.clone(),
                    trust_key,
                    hash,
                    command: command.to_string(),
                });
            }
        }
    }
    Ok(entries)
}

/// Render the bundled `hooks.json` template for deterministic golden tests.
/// Runtime trust is derived from the installed cache/source file instead.
#[cfg(test)]
fn codex_managed_hook_trust_entries(tracedecay_bin: &str) -> Result<Vec<CodexHookTrustEntry>> {
    let seed = codex_embedded_plugin_files()
        .into_iter()
        .find_map(|(relative, contents)| (relative == "hooks/hooks.json").then_some(contents))
        .ok_or_else(|| TraceDecayError::Config {
            message: "Codex plugin bundle is missing hooks/hooks.json".to_string(),
        })?;
    let rendered = codex_plugin_hooks(seed, tracedecay_bin)?;
    let value: serde_json::Value = serde_json::from_str(&rendered)?;
    codex_hook_trust_entries(&value)
}

fn codex_personal_marketplace_name(home: &Path) -> Result<String> {
    let marketplace_path = codex_personal_marketplace_path(home);
    let marketplace = load_json_file_strict(&marketplace_path)?;
    let name = marketplace
        .get("name")
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| TraceDecayError::Config {
            message: format!(
                "Codex marketplace at {} has no non-empty name",
                marketplace_path.display()
            ),
        })?;
    Ok(validate_codex_marketplace_name(name)?.to_string())
}

fn codex_runtime_hooks_path(home: &Path) -> PathBuf {
    let cached = codex_plugin_current_cached_install_dir(home).join("hooks/hooks.json");
    if cached.is_file() {
        cached
    } else {
        codex_plugin_install_dir(home).join("hooks/hooks.json")
    }
}

fn codex_installed_hook_trust_entries(home: &Path) -> Result<(String, Vec<CodexHookTrustEntry>)> {
    let marketplace_name = codex_personal_marketplace_name(home)?;
    let hooks_path = codex_runtime_hooks_path(home);
    if !hooks_path.is_file() {
        return Err(TraceDecayError::Config {
            message: format!("Codex hooks file not found at {}", hooks_path.display()),
        });
    }
    let hooks = load_json_file_strict(&hooks_path)?;
    let entries = codex_hook_trust_entries_for_marketplace(&hooks, &marketplace_name)?;
    Ok((marketplace_name, entries))
}

/// Safety valve: only auto-trust a hook whose command is byte-for-byte one of
/// the commands the generator emits for our own managed lifecycle hooks. A
/// prefix match is unsafe — `<quoted tracedecay> hook-codex-session-start &&
/// rm -rf ~` starts with our binary token yet smuggles an arbitrary command, so
/// it would get silently auto-trusted. Requiring full equality with a generated
/// command (`hook_command(bin, subcommand)` for each known subcommand) rejects
/// any appended, prepended, or altered payload.
fn codex_hook_command_invokes_tracedecay(command: &str, tracedecay_bin: &str) -> bool {
    CODEX_MANAGED_HOOKS
        .iter()
        .any(|hook| command == super::hook_command(tracedecay_bin, hook.subcommand))
}

/// Outcome of a hook-trust re-sync: how many hooks were recorded as trusted and
/// which (if any) were skipped by the safety valve and still need manual review.
struct CodexHookTrustSyncOutcome {
    trusted: usize,
    skipped: Vec<String>,
}

/// Record trust for the installed plugin's lifecycle hooks in
/// `~/.codex/config.toml` so Codex runs them without a manual `/hooks` approval.
///
/// Uses the marketplace identity and hook payload actually installed on disk,
/// pruning stale active/legacy-personal entries while preserving every other
/// plugin's and the user's own config. Hooks whose command does not exactly
/// match a generated `TraceDecay` command are skipped (see
/// [`codex_hook_command_invokes_tracedecay`]). An unreadable/unparseable
/// `config.toml` surfaces as `Err`, so callers leave it untouched and fall back
/// to printed guidance.
fn sync_codex_hook_trust(home: &Path, tracedecay_bin: &str) -> Result<CodexHookTrustSyncOutcome> {
    let (marketplace_name, entries) = codex_installed_hook_trust_entries(home)?;
    let config_path = codex_config_path(home);
    let contents = std::fs::read_to_string(&config_path).unwrap_or_default();
    let mut document =
        contents
            .parse::<DocumentMut>()
            .map_err(|error| TraceDecayError::Config {
                message: format!(
                    "failed to parse {} as TOML: {error}. Refusing to overwrite it.",
                    config_path.display()
                ),
            })?;
    let hooks = document
        .as_table_mut()
        .entry("hooks")
        .or_insert_with(|| Item::Table(Table::new()));
    let hooks = hooks
        .as_table_mut()
        .ok_or_else(|| TraceDecayError::Config {
            message: format!("[hooks] in {} is not a table", config_path.display()),
        })?;
    let state = hooks
        .entry("state")
        .or_insert_with(|| Item::Table(Table::new()));
    let state = state
        .as_table_mut()
        .ok_or_else(|| TraceDecayError::Config {
            message: format!("[hooks.state] in {} is not a table", config_path.display()),
        })?;

    // Drop trust for the active marketplace plus the legacy hard-coded
    // `personal` identity before adding the exact installed payload. Foreign
    // plugin and repo-local marketplace records remain untouched.
    let current_prefix = codex_plugin_hook_trust_prefix(&marketplace_name);
    let legacy_prefix = codex_plugin_hook_trust_prefix(CODEX_DEFAULT_MARKETPLACE_NAME);
    state.retain(|key, _| {
        !key.starts_with(&current_prefix)
            && (current_prefix == legacy_prefix || !key.starts_with(&legacy_prefix))
    });

    let mut trusted = 0usize;
    let mut skipped = Vec::new();
    for entry in &entries {
        if !codex_hook_command_invokes_tracedecay(&entry.command, tracedecay_bin) {
            skipped.push(entry.event_label.clone());
            continue;
        }
        let mut record = Table::new();
        record.insert("trusted_hash", value(entry.hash.clone()));
        state.insert(&entry.trust_key, Item::Table(record));
        trusted += 1;
    }

    write_codex_document(&config_path, &document)?;
    Ok(CodexHookTrustSyncOutcome { trusted, skipped })
}

fn write_codex_document(config_path: &Path, document: &DocumentMut) -> Result<()> {
    let backup = super::backup_config_file(config_path)?;
    safe_write_text_file(config_path, &document.to_string(), backup.as_deref())?;
    eprintln!("\x1b[32m✔\x1b[0m Wrote {}", config_path.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// Plugin activation
// ---------------------------------------------------------------------------

/// Parse `~/.codex/config.toml` for editing, treating an absent file as empty
/// and refusing to overwrite one that is not valid TOML.
fn codex_config_document(config_path: &Path) -> Result<DocumentMut> {
    let contents = std::fs::read_to_string(config_path).unwrap_or_default();
    contents
        .parse::<DocumentMut>()
        .map_err(|error| TraceDecayError::Config {
            message: format!(
                "failed to parse {} as TOML: {error}. Refusing to overwrite it.",
                config_path.display()
            ),
        })
}

/// The `plugins.<plugin>@<marketplace>` config key Codex reads to decide whether
/// a plugin is enabled.
fn codex_plugin_activation_key(marketplace_name: &str) -> String {
    format!("{CODEX_PLUGIN_ACTIVATION_KEY_PREFIX}{marketplace_name}")
}

/// Record (or clear) `[plugins."tracedecay@<marketplace>"] enabled = <enabled>`
/// in `~/.codex/config.toml`, returning the activation key that was written.
///
/// Fail-safe by construction: an unparseable config, a `[plugins]` that is not
/// a table, or an existing `tracedecay@…` record that is not a table are all
/// refused rather than rewritten, so an unrecognised Codex schema degrades to
/// the operator-run `codex plugin add` step instead of corrupting the file.
/// Every other plugin's record — and the user's own config — is preserved.
fn codex_set_plugin_activation(home: &Path, enabled: bool) -> Result<String> {
    let marketplace_name = codex_personal_marketplace_name(home)?;
    let key = codex_plugin_activation_key(&marketplace_name);
    let config_path = codex_config_path(home);
    let mut document = codex_config_document(&config_path)?;
    let plugins = document
        .as_table_mut()
        .entry("plugins")
        .or_insert_with(|| Item::Table(Table::new()));
    let plugins = plugins
        .as_table_mut()
        .ok_or_else(|| TraceDecayError::Config {
            message: format!("[plugins] in {} is not a table", config_path.display()),
        })?;
    if let Some(existing) = plugins.get(&key)
        && existing.as_table_like().is_none()
    {
        return Err(TraceDecayError::Config {
            message: format!(
                "[plugins.\"{key}\"] in {} is not a table; refusing to overwrite it",
                config_path.display()
            ),
        });
    }
    let record = plugins
        .entry(&key)
        .or_insert_with(|| Item::Table(Table::new()));
    let record = record
        .as_table_like_mut()
        .ok_or_else(|| TraceDecayError::Config {
            message: format!(
                "[plugins.\"{key}\"] in {} is not a table",
                config_path.display()
            ),
        })?;
    record.insert("enabled", value(enabled));
    write_codex_document(&config_path, &document)?;
    Ok(key)
}

/// Drop every `tracedecay@<marketplace>` activation record from a parsed config,
/// reporting whether anything changed. Foreign plugin records stay untouched.
fn codex_remove_plugin_activation(document: &mut DocumentMut) -> bool {
    let Some(plugins) = document.get_mut("plugins").and_then(Item::as_table_mut) else {
        return false;
    };
    let previous_len = plugins.len();
    plugins.retain(|key, _| !key.starts_with(CODEX_PLUGIN_ACTIVATION_KEY_PREFIX));
    let changed = plugins.len() != previous_len;
    if plugins.is_empty() {
        document.as_table_mut().remove("plugins");
    }
    changed
}

/// Everything Codex needs to treat the staged bundle as an installed, enabled
/// plugin: the marketplace entry it resolves the source from, the cached
/// version directory it actually loads (`codex plugin add` materialises the
/// same copy), and the `enabled = true` record in `config.toml`. Idempotent.
fn codex_activate_plugin(home: &Path, tracedecay_bin: &str) -> Result<String> {
    install_codex_marketplace_entry(
        &codex_personal_marketplace_path(home),
        CODEX_DEFAULT_MARKETPLACE_NAME,
        "Personal",
        "./plugins/tracedecay",
    )?;
    install_codex_cached_plugin(home, tracedecay_bin)?;
    codex_set_plugin_activation(home, true)
}

/// Activate the installed plugin and report the outcome, returning the
/// marketplace name when activation was left to the operator.
fn announce_codex_plugin_activation(home: &Path, tracedecay_bin: &str) -> Option<String> {
    match codex_activate_plugin(home, tracedecay_bin) {
        Ok(key) => {
            eprintln!(
                "\x1b[32m✔\x1b[0m Activated Codex plugin {key} in {}",
                codex_config_path(home).display()
            );
            None
        }
        Err(error) => {
            eprintln!("  Could not activate the Codex plugin automatically: {error}");
            Some(codex_cached_marketplace_name(home))
        }
    }
}

/// Why a `config.toml` cannot carry an activation record, or `None` when
/// TraceDecay can write one. Read-only: the doctor and preflight need this fact
/// without staging anything.
fn codex_unwritable_activation_reason(home: &Path) -> Option<String> {
    let config_path = codex_config_path(home);
    let document = match codex_config_document(&config_path) {
        Ok(document) => document,
        Err(error) => return Some(error.to_string()),
    };
    let plugins = document.get("plugins")?;
    let Some(plugins) = plugins.as_table_like() else {
        return Some(format!(
            "[plugins] in {} is not a table",
            config_path.display()
        ));
    };
    plugins
        .iter()
        .find(|(key, item)| {
            key.starts_with(CODEX_PLUGIN_ACTIVATION_KEY_PREFIX) && item.as_table_like().is_none()
        })
        .map(|(key, _)| {
            format!(
                "[plugins.\"{key}\"] in {} is not a table",
                config_path.display()
            )
        })
}

/// Whether any TraceDecay-owned *registration* record survives in the Codex
/// host surface — precisely the set [`uninstall_codex_config`] and
/// [`remove_codex_marketplace_entry`] clear: the `[hooks.state]` trust keys, the
/// `[mcp_servers.tracedecay]` entry, the `[plugins."tracedecay@…"]` activation
/// records, and the personal marketplace entry.
///
/// The deployed plugin source bundle and its cached install directories are
/// deliberately excluded: those are artifact-layer deployments, not registration,
/// and they outlive deactivation. `Err(())` marks a host surface TraceDecay
/// cannot read, which callers report as corrupt rather than merely repairable.
fn codex_registration_residue(home: &Path) -> std::result::Result<bool, ()> {
    let config = load_toml_file(&codex_config_path(home)).map_err(|_| ())?;
    let hook_trust_residue = config
        .get("hooks")
        .and_then(toml::Value::as_table)
        .and_then(|hooks| hooks.get("state"))
        .and_then(toml::Value::as_table)
        .is_some_and(|state| {
            state
                .keys()
                .any(|key| key.starts_with(CODEX_PLUGIN_ACTIVATION_KEY_PREFIX))
        });
    let mcp_residue = config
        .get("mcp_servers")
        .and_then(toml::Value::as_table)
        .is_some_and(|servers| servers.contains_key("tracedecay"));
    let activation_residue = config
        .get("plugins")
        .and_then(toml::Value::as_table)
        .is_some_and(|plugins| {
            plugins
                .keys()
                .any(|key| key.starts_with(CODEX_PLUGIN_ACTIVATION_KEY_PREFIX))
        });
    let marketplace =
        load_json_file_strict(&codex_personal_marketplace_path(home)).map_err(|_| ())?;
    let marketplace_residue = marketplace
        .get("plugins")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|plugins| {
            plugins.iter().any(|entry| {
                entry.get("name").and_then(serde_json::Value::as_str) == Some("tracedecay")
            })
        });
    Ok(hook_trust_residue || mcp_residue || activation_residue || marketplace_residue)
}

/// Whether Codex would load this plugin: some `tracedecay@<marketplace>` record
/// says `enabled = true` and the cached bundle that record points at exists.
/// `Err(())` marks a config TraceDecay cannot read, which the caller reports as
/// a corrupt registration rather than a merely repairable one.
fn codex_plugin_activation_state(home: &Path) -> std::result::Result<bool, ()> {
    let config_path = codex_config_path(home);
    if !config_path.exists() {
        return Ok(false);
    }
    let config = load_toml_file(&config_path).map_err(|_| ())?;
    let Some(plugins) = config.get("plugins") else {
        return Ok(false);
    };
    let Some(plugins) = plugins.as_table() else {
        return Err(());
    };
    let enabled = plugins.iter().any(|(key, record)| {
        key.starts_with(CODEX_PLUGIN_ACTIVATION_KEY_PREFIX)
            && record
                .get("enabled")
                .and_then(toml::Value::as_bool)
                .unwrap_or(false)
    });
    Ok(enabled && !codex_plugin_cached_install_dirs(home).is_empty())
}

fn codex_hook_state_table_is_explicit(contents: &str) -> bool {
    contents.lines().any(|line| line.trim() == "[hooks.state]")
}

/// Auto-trust the installed plugin's hooks, printing a concise confirmation on
/// full success and falling back to [`print_hook_trust_guidance`] whenever a
/// hook is skipped by the safety valve or the config could not be written.
fn announce_codex_hook_trust(home: &Path, tracedecay_bin: &str) {
    let config_path = codex_config_path(home);
    match sync_codex_hook_trust(home, tracedecay_bin) {
        Ok(outcome) if outcome.skipped.is_empty() => {
            eprintln!(
                "\x1b[32m✔\x1b[0m Trusted {} Codex hook(s) in {}",
                outcome.trusted,
                config_path.display()
            );
        }
        Ok(outcome) => {
            if outcome.trusted > 0 {
                eprintln!(
                    "\x1b[32m✔\x1b[0m Trusted {} Codex hook(s) in {}",
                    outcome.trusted,
                    config_path.display()
                );
            }
            eprintln!(
                "  Skipped auto-trust for {} (command does not invoke the tracedecay binary).",
                outcome.skipped.join(", ")
            );
            print_hook_trust_guidance();
        }
        Err(err) => {
            eprintln!("  Could not auto-trust Codex hooks: {err}");
            print_hook_trust_guidance();
        }
    }
}

/// Classify the recorded Codex trust state for the personal plugin's hooks by
/// comparing each expected [`CodexHookTrustEntry`] against `config.toml`.
fn codex_plugin_hook_trust_state(
    config: &toml::Value,
    entries: &[CodexHookTrustEntry],
) -> CodexHookTrustState {
    // A missing [hooks.state] table is just "nothing trusted yet" — treat it
    // as empty so one pipeline produces the missing list either way.
    let empty = toml::value::Table::new();
    let state = config
        .get("hooks")
        .and_then(|hooks| hooks.get("state"))
        .and_then(|state| state.as_table())
        .unwrap_or(&empty);

    let mut missing = Vec::new();
    let mut modified = Vec::new();
    for entry in entries {
        match state
            .get(&entry.trust_key)
            .and_then(|record| record.get("trusted_hash"))
            .and_then(|hash| hash.as_str())
        {
            None => missing.push(entry.event_label.clone()),
            Some(stored) if stored == entry.hash => {}
            Some(_) => modified.push(entry.event_label.clone()),
        }
    }

    if !missing.is_empty() {
        CodexHookTrustState::Missing(missing)
    } else if !modified.is_empty() {
        CodexHookTrustState::Modified(modified)
    } else {
        CodexHookTrustState::Trusted
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
    let existing_name = marketplace.get("name").and_then(|value| value.as_str());
    if let Some(existing_name) = existing_name {
        validate_codex_marketplace_name(existing_name)?;
    }
    let has_tracedecay_entry = marketplace
        .get("plugins")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|plugins| {
            plugins.iter().any(|entry| {
                entry.get("name").and_then(|value| value.as_str()) == Some("tracedecay")
            })
        });
    let should_write_identity =
        existing_name.is_none() || (existing_name == Some("caveman-home") && has_tracedecay_entry);
    if should_write_identity {
        marketplace["name"] = json!(marketplace_name);
    }
    if !marketplace
        .get("interface")
        .is_some_and(serde_json::Value::is_object)
    {
        marketplace["interface"] = json!({});
    }
    if should_write_identity
        || marketplace["interface"]
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
    let effective_marketplace_name = marketplace
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(marketplace_name)
        .to_string();
    safe_write_json_file(marketplace_path, &marketplace, None)?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Added tracedecay to Codex {effective_marketplace_name} marketplace at {}",
        marketplace_path.display()
    );
    Ok(())
}

fn uninstall_codex_plugin(home: &Path) -> Result<()> {
    crate::automation::agent_targets::remove_managed_agents(&home.join(".codex/agents"))?;
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
        super::safe_remove_host_file(&skills_dir).map_err(|e| TraceDecayError::Config {
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
            super::safe_remove_host_file(&file).map_err(|e| TraceDecayError::Config {
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
        super::safe_remove_host_file(install_dir).map_err(|e| TraceDecayError::Config {
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
            super::safe_remove_host_file(&path).ok();
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

fn uninstall_codex_config(config_path: &Path) -> Result<()> {
    if !config_path.exists() {
        return Ok(());
    }
    let contents =
        std::fs::read_to_string(config_path).map_err(|error| TraceDecayError::Config {
            message: format!("failed to read {}: {error}", config_path.display()),
        })?;
    let mut document =
        contents
            .parse::<DocumentMut>()
            .map_err(|error| TraceDecayError::Config {
                message: format!(
                    "failed to parse {} as TOML: {error}. Refusing to overwrite it.",
                    config_path.display()
                ),
            })?;
    let mut changed = false;
    if let Some(hooks) = document.get_mut("hooks").and_then(Item::as_table_mut) {
        if let Some(state) = hooks.get_mut("state").and_then(Item::as_table_mut) {
            let previous_len = state.len();
            state.retain(|key, _| !key.starts_with("tracedecay@"));
            changed |= state.len() != previous_len;
            if state.is_empty() {
                hooks.remove("state");
            }
        }
        if hooks.is_empty() {
            document.as_table_mut().remove("hooks");
        }
    }
    if let Some(servers) = document.get_mut("mcp_servers").and_then(Item::as_table_mut) {
        changed |= servers.remove("tracedecay").is_some();
        if servers.is_empty() {
            document.as_table_mut().remove("mcp_servers");
        }
    }
    changed |= codex_remove_plugin_activation(&mut document);
    if !changed {
        return Ok(());
    }
    let backup = super::backup_config_file(config_path)?;
    let updated = document.to_string();
    if updated.trim().is_empty() {
        super::safe_remove_host_file(config_path).map_err(|error| TraceDecayError::Config {
            message: format!("failed to remove {}: {error}", config_path.display()),
        })?;
        tracedecay_application::sync_parent_directory(
            config_path,
            tracedecay_application::DirectorySyncPolicy::TolerateUnsupported,
        )
        .map_err(|error| TraceDecayError::Config {
            message: format!(
                "failed to durably remove {}: {error}",
                config_path.display()
            ),
        })?;
    } else {
        safe_write_text_file(config_path, &updated, backup.as_deref())?;
    }
    Ok(())
}

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
        super::safe_remove_host_file(hooks_path).ok();
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
        super::safe_remove_host_file(agents_md).ok();
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
        Some(crate::PRODUCT_VERSION) => dc.pass("Codex plugin version matches tracedecay"),
        Some(version) => dc.warn(&format!(
            "Codex plugin version {version} does not match tracedecay {} — run `tracedecay update-plugin`",
            crate::PRODUCT_VERSION
        )),
        None => dc.warn("Codex plugin manifest does not contain a version"),
    }

    let mcp_path = plugin_dir.join(".mcp.json");
    let mcp = load_json_file(&mcp_path);
    if codex_mcp_timeouts_current(&mcp) {
        dc.pass(&format!(
            "Codex plugin MCP server registered with managed timeouts in {}",
            mcp_path.display()
        ));
    } else {
        dc.fail(&format!(
            "Codex plugin MCP server missing or has stale timeouts in {} — run `tracedecay update-plugin`",
            mcp_path.display()
        ));
    }
    let hooks_path = plugin_dir.join("hooks/hooks.json");
    if let Some(config_path) = policy.hook_trust_config_path(home) {
        match codex_personal_marketplace_name(home) {
            Ok(marketplace_name) => {
                doctor_check_hooks(dc, &hooks_path, &config_path, &marketplace_name);
            }
            Err(err) => dc.warn(&format!(
                "Cannot verify Codex hook trust without the installed marketplace identity: {err}"
            )),
        }
    } else if hooks_path.exists() {
        dc.warn(&format!(
            "repo-local Codex bundle unexpectedly ships lifecycle hooks in {} — run `tracedecay install --local --agent codex` to refresh it",
            hooks_path.display()
        ));
    }
}

/// Check hooks.json registers the tracedecay lifecycle hooks, and report Codex
/// hook trust state from the user-level config.
fn doctor_check_hooks(
    dc: &mut DoctorCounters,
    hooks_path: &Path,
    config_path: &Path,
    marketplace_name: &str,
) {
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
    // Hash the on-disk hooks.json exactly as Codex would, then compare against
    // the trust records in config.toml to distinguish trusted / missing / stale.
    let entries = match codex_hook_trust_entries_for_marketplace(&hooks, marketplace_name) {
        Ok(entries) => entries,
        Err(err) => {
            dc.warn(&format!(
                "Cannot hash Codex hooks for trust verification: {err}"
            ));
            return;
        }
    };
    match load_toml_file(config_path) {
        Ok(config) => match codex_plugin_hook_trust_state(&config, &entries) {
            CodexHookTrustState::Trusted
                if std::fs::read_to_string(config_path)
                    .is_ok_and(|contents| codex_hook_state_table_is_explicit(&contents)) =>
            {
                dc.pass(&format!(
                    "Codex hook trust entries recorded and current in {}",
                    config_path.display()
                ));
            }
            CodexHookTrustState::Trusted => dc.warn(&format!(
                "Codex hook trust records in {} lack an explicit [hooks.state] table, so Codex still requests review; run `tracedecay update-plugin` to repair and auto-trust them",
                config_path.display()
            )),
            CodexHookTrustState::Missing(missing) => dc.info(&format!(
                "Codex skips untrusted command hooks — missing trust for {} in {}; run `tracedecay update-plugin` (or `/hooks` in Codex) to trust them",
                missing.join(", "),
                config_path.display()
            )),
            CodexHookTrustState::Modified(modified) => dc.warn(&format!(
                "Codex hook trust is stale for {} in {} — the hook content changed since it was trusted, so Codex now skips it; run `tracedecay update-plugin` to re-trust",
                modified.join(", "),
                config_path.display()
            )),
        },
        Err(_) => dc.info(
            "Codex skips untrusted command hooks — run `tracedecay update-plugin` (or `/hooks` in Codex) to trust the tracedecay hooks",
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
    if !crate::ports::hook_runtime::memory_injection_enabled() {
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
