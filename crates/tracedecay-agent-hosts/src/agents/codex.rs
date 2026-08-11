// Rust guideline compliant 2025-10-17
//! `OpenAI` Codex CLI agent integration.
//!
//! Stages the TraceDecay plugin source for Codex. Codex itself owns cache,
//! activation, and hook-trust mutations through its native plugin commands.
//!
//! # Ruling: what is driven, what stays manual
//!
//! Codex's plugin lifecycle is **interactive-only** (`/plugin marketplace add`,
//! `/plugin install`, `/reload-plugins` all run inside a session), so plugin
//! install and activation stay **manual**. TraceDecay stages the plugin source
//! tree and the personal marketplace entry and stops; it never drives a plugin
//! install, and it never writes `~/.codex/config.toml` — Codex owns the
//! `tracedecay@<marketplace>` activation keys and the `[hooks.state]` trust
//! hashes recorded there. `install_codex_plugin`,
//! `activate_deployed_host_registration`, and `update_plugin` all reflect that:
//! they stage and then report host-native guidance rather than acting.
//!
//! Codex's **MCP registry** is the one part that *is* documented as
//! non-interactive (`codex mcp add`/`remove`/`list`/`get`). That is a host
//! capability TraceDecay drives rather than emulates, but only for the MCP-only
//! (non-plugin) component set, where nothing else would register the server.
//! See [`mcp_registry`] for the whole of that adoption and its reasoning.
//!
//! Note on rollback ownership: `CodexIntegration::host_registration_paths`
//! already lists `~/.codex/config.toml` and its backup, so the component-set
//! transaction stages that file before the registry command runs and can
//! restore the pre-command document if the effect is rejected.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::json;
use tracedecay_domain::canonical_sha256;

use crate::errors::{Result, TraceDecayError};

use super::{
    AgentIntegration, DeferredUserAction, DoctorCounters, HealthcheckContext, InstallContext,
    InstallScope, NonInteractiveInstallOutcome, UpdatePluginOutcome, config_backup_path,
    load_json_file, load_json_file_strict, load_toml_file, safe_write_json_file,
    safe_write_text_file,
};

/// The prefix every Codex activation key for this plugin starts with. TraceDecay
/// reads this host-native state but never writes it.
const CODEX_PLUGIN_ACTIVATION_KEY_PREFIX: &str = "tracedecay@";

mod mcp_registry;
mod retired_entrypoints;

/// `OpenAI` Codex CLI agent.
pub struct CodexIntegration;

impl AgentIntegration for CodexIntegration {
    fn name(&self) -> &'static str {
        "Codex CLI"
    }

    fn id(&self) -> &'static str {
        "codex"
    }

    fn supports_local_install(&self) -> bool {
        true
    }

    fn preflight_non_interactive_install(
        &self,
        ctx: &InstallContext,
    ) -> Result<NonInteractiveInstallOutcome> {
        codex_non_interactive_install_state(&ctx.home, &ctx.tracedecay_bin, Vec::new())
    }

    fn interactive_activation_guidance(&self) -> Option<String> {
        Some(
            "Codex activates plugin caches natively; run `codex plugin add tracedecay@personal` after staging the source package.".to_string(),
        )
    }

    fn interactive_removal_guidance(&self) -> Option<String> {
        Some(
            "Codex owns plugin removal; run `codex plugin remove tracedecay@personal`, then re-run TraceDecay to remove its staged source package.".to_string(),
        )
    }

    fn prepare_non_interactive_install(
        &self,
        ctx: &InstallContext,
    ) -> Result<NonInteractiveInstallOutcome> {
        install_codex_plugin(&ctx.home, &ctx.tracedecay_bin)?;
        codex_non_interactive_install_state(
            &ctx.home,
            &ctx.tracedecay_bin,
            vec![
                codex_plugin_manifest_path(&ctx.home),
                codex_personal_marketplace_path(&ctx.home),
            ],
        )
    }

    fn activate_project_host_component_registration(
        &self,
        _components: &[super::host_bundle_v2::HostBundleComponentV1],
        ctx: &InstallContext,
        project_path: &Path,
    ) -> Result<()> {
        for path in [
            codex_repo_plugin_install_dir(project_path).join(".codex-plugin/plugin.json"),
            codex_repo_plugin_install_dir(project_path).join(".mcp.json"),
            codex_repo_marketplace_path(project_path),
        ] {
            super::ensure_project_local_safe_path(project_path, &path)?;
        }
        install_codex_repo_plugin(&ctx.home, project_path, &ctx.tracedecay_bin)
    }

    fn project_host_component_registration_paths(
        &self,
        _components: &[super::host_bundle_v2::HostBundleComponentV1],
        home: &Path,
        project_path: &Path,
    ) -> Result<Vec<PathBuf>> {
        codex_project_registration_paths(home, project_path)
    }

    fn deactivate_project_host_component_registration(
        &self,
        _components: &[super::host_bundle_v2::HostBundleComponentV1],
        ctx: &InstallContext,
        project_path: &Path,
    ) -> Result<()> {
        let local = InstallContext {
            home: ctx.home.clone(),
            tracedecay_bin: ctx.tracedecay_bin.clone(),
            tool_permissions: ctx.tool_permissions.clone(),
            project_root: Some(project_path.to_path_buf()),
            dashboard: ctx.dashboard,
        };
        uninstall_codex_repo_plugin_if_present(&local)
    }

    fn update_plugin(&self, ctx: &InstallContext) -> Result<UpdatePluginOutcome> {
        let cached_install_present =
            codex_exact_cache_manifest_path(&ctx.home)?.is_some_and(|path| path.is_file());
        let source_present = codex_plugin_manifest_path(&ctx.home).exists();
        let mut staged = Vec::new();
        if cached_install_present || source_present {
            // Codex owns its cache lifecycle. Refresh the marketplace source
            // it will consume, but never materialise or replace a cache entry
            // on the host's behalf.
            staged.push(install_codex_personal_bootstrap(
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
                staged.push(repo_dir);
            }
        }

        if staged.is_empty() {
            return Ok(UpdatePluginOutcome::NotInstalled);
        }
        let marketplace_name = codex_cached_marketplace_name(&ctx.home);
        Ok(UpdatePluginOutcome::DeferredUserAction(
            DeferredUserAction {
                remediation: format!(
                    "Codex plugin source is staged. Run `codex plugin add tracedecay@{marketplace_name}` to install or reinstall it, then re-trust any changed hooks."
                ),
                staged_paths: staged,
            },
        ))
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

        // Codex owns activation. The staged source bundle alone is not an
        // installed plugin, but it truthfully reports a repairable host-native
        // activation rather than pretending TraceDecay can complete it.
        match codex_registration_residue(&ctx.home) {
            Ok(false) => return State::Missing,
            Ok(true) => {}
            Err(()) => return State::Corrupt,
        }

        // A deployed source bundle alone is not activation. Codex's own
        // readback for "this plugin is installed and enabled" is its native
        // cache plus `enabled = true` in `config.toml`; TraceDecay reads this
        // state but leaves cache materialisation to the Codex CLI.
        match codex_plugin_activation_state(&ctx.home, None) {
            Ok(true) => State::Current,
            Ok(false) => State::Repairable,
            Err(()) => State::Corrupt,
        }
    }

    fn host_component_registration_for_lifecycle(
        &self,
        component: super::host_bundle_v2::HostBundleComponentV1,
        ctx: &HealthcheckContext,
        install: &InstallContext,
    ) -> super::host_bundle_v2::HostBundleRegistrationStateV1 {
        use super::host_bundle_v2::HostBundleRegistrationStateV1 as State;

        match self.host_component_registration(component, ctx) {
            State::Current => {
                match codex_plugin_activation_state(&ctx.home, Some(&install.tracedecay_bin)) {
                    Ok(true) => State::Current,
                    Ok(false) => State::Repairable,
                    Err(()) => State::Corrupt,
                }
            }
            state => state,
        }
    }

    fn is_detected(&self, home: &Path) -> bool {
        home.join(".codex").is_dir()
            || !codex_plugin_cached_install_dirs(home).is_empty()
            || codex_plugin_manifest_path(home).exists()
    }

    fn primary_config_path(&self, home: &Path) -> Option<std::path::PathBuf> {
        let current_cache =
            codex_plugin_current_cached_install_dir(home).join(".codex-plugin/plugin.json");
        Some(if current_cache.is_file() {
            current_cache
        } else {
            codex_plugin_manifest_path(home)
        })
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
        paths.sort();
        paths.dedup();
        paths
    }

    fn host_component_registration_paths(
        &self,
        components: &[super::host_bundle_v2::HostBundleComponentV1],
        home: &Path,
    ) -> Vec<PathBuf> {
        let _ = components;
        self.host_registration_paths(home)
    }

    fn activate_deployed_host_registration(&self, ctx: &InstallContext) -> Result<()> {
        if codex_plugin_is_natively_active(&ctx.home, Some(&ctx.tracedecay_bin))? {
            return Ok(());
        }
        let marketplace_name = codex_cached_marketplace_name(&ctx.home);
        Err(TraceDecayError::Config {
            message: format!(
                "Codex activation is host-native. Run `codex plugin add tracedecay@{marketplace_name}` after the source package is staged."
            ),
        })
    }

    fn deactivate_deployed_host_registration(&self, ctx: &InstallContext) -> Result<()> {
        if !codex_plugin_is_natively_active(&ctx.home, Some(&ctx.tracedecay_bin))? {
            return Ok(());
        }
        let marketplace_name = codex_cached_marketplace_name(&ctx.home);
        Err(TraceDecayError::Config {
            message: format!(
                "Codex activation is host-native. Run `codex plugin remove tracedecay@{marketplace_name}` before removing the staged source package."
            ),
        })
    }

    /// Split by component: the MCP-only set is driven through Codex's own
    /// non-interactive MCP registry; anything carrying `Core` keeps the
    /// host-native (manual) plugin path.
    ///
    /// Codex publishes `codex mcp add`/`remove` as non-interactive commands, so
    /// the MCP route is a host capability TraceDecay drives rather than
    /// emulates. Codex's *plugin* lifecycle has no such command — install and
    /// activation happen inside a session — so it stays manual, and this method
    /// still forwards a `Core`-bearing set to
    /// `activate_deployed_host_registration`, which reports the
    /// host-native guidance instead of acting. That split is deliberate: a
    /// plugin install already carries the MCP route inside its bundled
    /// `.mcp.json`, and adding a standalone server beside it would give the
    /// operator two identical tracedecay servers, one of them outside
    /// `codex plugin` management. See [`mcp_registry`] for the full ruling.
    fn activate_deployed_host_component_registration(
        &self,
        components: &[super::host_bundle_v2::HostBundleComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        if mcp_registry::is_mcp_only_component_set(components) {
            let codex_cli = mcp_registry::require_codex_cli()?;
            return mcp_registry::codex_mcp_add_with(&codex_cli, &ctx.home, &ctx.tracedecay_bin);
        }
        if components.contains(&super::host_bundle_v2::HostBundleComponentV1::Core) {
            return self.activate_deployed_host_registration(ctx);
        }
        Ok(())
    }

    /// Mirrors `activate_deployed_host_component_registration`: the
    /// MCP-only set is removed through Codex's own `codex mcp remove`, and a
    /// `Core`-bearing set keeps the manual plugin-removal guidance.
    fn deactivate_deployed_host_component_registration(
        &self,
        components: &[super::host_bundle_v2::HostBundleComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        if mcp_registry::is_mcp_only_component_set(components) {
            let codex_cli = mcp_registry::require_codex_cli()?;
            return mcp_registry::codex_mcp_remove_with(&codex_cli, &ctx.home);
        }
        if components.contains(&super::host_bundle_v2::HostBundleComponentV1::Core) {
            return self.deactivate_deployed_host_registration(ctx);
        }
        Ok(())
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        !codex_plugin_cached_install_dirs(home).is_empty()
            || codex_plugin_manifest_path(home).exists()
    }
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
    home.join(".codex/plugins/tracedecay")
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

fn codex_exact_cache_manifest_path(home: &Path) -> Result<Option<PathBuf>> {
    let marketplace_name =
        codex_exact_personal_marketplace_name(home).map_err(|()| TraceDecayError::Config {
            message: format!(
                "could not read exact Codex marketplace identity at {}",
                codex_personal_marketplace_path(home).display()
            ),
        })?;
    Ok(marketplace_name.map(|marketplace_name| {
        codex_plugin_cached_root(home, &marketplace_name)
            .join(crate::PRODUCT_VERSION)
            .join(".codex-plugin/plugin.json")
    }))
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
    let install_dir = install_codex_personal_bootstrap(home, tracedecay_bin)?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Staged Codex plugin source at {}",
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
        CODEX_GLOBAL_PLUGIN_SOURCE_PATH,
    )?;
    Ok(install_dir)
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

fn codex_project_registration_paths(home: &Path, project_path: &Path) -> Result<Vec<PathBuf>> {
    let install_dir = codex_repo_plugin_install_dir(project_path);
    super::ensure_project_local_safe_path(project_path, &install_dir)?;

    let mut paths = codex_embedded_plugin_files()
        .into_iter()
        .filter(|(relative, _)| *relative != "hooks/hooks.json")
        .map(|(relative, _)| install_dir.join(relative))
        .collect::<Vec<_>>();

    match std::fs::symlink_metadata(&install_dir) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            paths.extend(super::collect_regular_files(&install_dir).map_err(|error| {
                TraceDecayError::Config {
                    message: format!(
                        "failed to inventory Codex project plugin {}: {error}",
                        install_dir.display()
                    ),
                }
            })?);
        }
        Ok(_) => {
            return Err(TraceDecayError::Config {
                message: format!(
                    "refusing to inventory unsafe Codex project plugin path {}",
                    install_dir.display()
                ),
            });
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(TraceDecayError::Config {
                message: format!(
                    "failed to inspect Codex project plugin {}: {error}",
                    install_dir.display()
                ),
            });
        }
    }

    let profile_root = crate::automation::skill_targets::profile_root_for_agent_home(home);
    let active_skills = crate::automation::skill_targets::load_active_managed_skills_for_target(
        &profile_root,
        crate::automation::skill_targets::SkillInstallTarget::Codex,
    )?;
    let overlay_root = install_dir.join("skills/agent-managed");
    if !active_skills.is_empty() {
        paths.push(overlay_root.join(".tracedecay-managed-skills.json"));
    }
    for skill in active_skills {
        crate::automation::managed_skills::validate_managed_support_files(&skill.support_files)?;
        let package_dir = overlay_root.join(&skill.metadata.id);
        paths.push(package_dir.join("SKILL.md"));
        paths.extend(
            skill
                .support_files
                .into_iter()
                .map(|support| package_dir.join(support.path)),
        );
    }
    paths.push(codex_repo_marketplace_path(project_path));
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// The scope contract for a rendered Codex plugin bundle, in one place.
///
/// A global bundle ships lifecycle hooks (declared in the manifest and trusted
/// later by Codex itself), invokes `serve` without an explicit project path,
/// and carries lifecycle hooks. A repo-local bundle ships no hooks, invokes
/// `serve --path .` with no env, and stays free of user-profile state. The
/// bundle writer, manifest/MCP renderers, and doctor all consume this type
/// instead of re-encoding the scope as ad-hoc conditionals.
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
    ///
    /// Both arms build on [`CODEX_MCP_SERVER_ARGS`] so this writer and the
    /// CLI-driven registration in [`mcp_registry`] launch the same server.
    fn mcp_args(self) -> serde_json::Value {
        // Scope adds to the shared base; it never restates `serve`.
        let scoped: &[&str] = match self.scope {
            InstallScope::Global => &[],
            InstallScope::ProjectLocal => &["--path", "."],
        };
        serde_json::Value::Array(
            CODEX_MCP_SERVER_ARGS
                .iter()
                .chain(scoped.iter())
                .map(|arg| serde_json::Value::String((*arg).to_string()))
                .collect(),
        )
    }

    /// The `env` baked into the bundle's `.mcp.json`; `None` strips the key.
    fn mcp_env(self) -> Option<serde_json::Value> {
        match self.scope {
            InstallScope::Global => Some(codex_mcp_server_env_object()),
            InstallScope::ProjectLocal => None,
        }
    }

    /// Where Codex records trust for this bundle's hooks — `None` for scopes
    /// that ship no hooks and therefore have no trust surface.
    fn hook_trust_config_path(self, home: &Path) -> Option<PathBuf> {
        self.include_hooks().then(|| codex_config_path(home))
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
    super::retired_memory_digest::remove_state(&profile_root)?;
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
        timeout_secs: 5,
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
const CODEX_DEFAULT_MARKETPLACE_NAME: &str = "personal";
const CODEX_GLOBAL_PLUGIN_SOURCE_PATH: &str = "./.codex/plugins/tracedecay";
const CODEX_MCP_STARTUP_TIMEOUT_SECS: u64 = 120;
const CODEX_MCP_TOOL_TIMEOUT_SECS: u64 = 900;

/// Arguments the tracedecay MCP server is launched with under Codex's global
/// scope.
///
/// Shared by the plugin bundle's `.mcp.json` writer
/// ([`CodexBundlePolicy::mcp_args`]) and the CLI-driven MCP-only registration
/// ([`mcp_registry::codex_mcp_add_with`], which passes them after `--`), so the
/// two spellings of the same server cannot drift apart. The project-local
/// bundle appends its own `--path .` on top of this base rather than restating
/// `serve`.
const CODEX_MCP_SERVER_ARGS: &[&str] = &["serve"];

/// Environment the global-scope server is launched with, in the same shared
/// role as [`CODEX_MCP_SERVER_ARGS`]: the bundle writer renders it as the
/// `.mcp.json` `env` object and the registry driver renders one `--env
/// KEY=VALUE` flag per entry.
const CODEX_MCP_SERVER_ENV: &[(&str, &str)] = &[("TRACEDECAY_ENABLE_GLOBAL_DB", "1")];

/// [`CODEX_MCP_SERVER_ENV`] as the JSON object the plugin bundle embeds.
fn codex_mcp_server_env_object() -> serde_json::Value {
    let mut env = serde_json::Map::new();
    for (key, value) in CODEX_MCP_SERVER_ENV {
        env.insert(
            (*key).to_string(),
            serde_json::Value::String((*value).to_string()),
        );
    }
    serde_json::Value::Object(env)
}

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
#[derive(Debug, Clone)]
struct CodexHookTrustEntry {
    event_label: String,
    trust_key: String,
    hash: String,
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

/// Whether Codex's host-native registration records identify TraceDecay. A
/// missing config or personal marketplace means no registration; unreadable
/// existing files are corrupt rather than silently treated as absent.
fn codex_registration_residue(home: &Path) -> std::result::Result<bool, ()> {
    let config_path = codex_config_path(home);
    let config = if config_path.exists() {
        load_toml_file(&config_path).map_err(|_| ())?
    } else {
        toml::Value::Table(Default::default())
    };
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
    let marketplace_path = codex_personal_marketplace_path(home);
    let marketplace_residue = if marketplace_path.exists() {
        let marketplace = load_json_file_strict(&marketplace_path).map_err(|_| ())?;
        marketplace
            .get("plugins")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|plugins| {
                plugins.iter().any(|entry| {
                    entry.get("name").and_then(serde_json::Value::as_str) == Some("tracedecay")
                })
            })
    } else {
        false
    };
    Ok(hook_trust_residue || mcp_residue || activation_residue || marketplace_residue)
}

/// Whether Codex would load this exact personal-catalog plugin: its catalog
/// source, activation key, and versioned cache must all name the same
/// marketplace and current TraceDecay version.
/// `Err(())` marks a config TraceDecay cannot read, which the caller reports as
/// a corrupt registration rather than a merely repairable one.
fn codex_plugin_activation_state(
    home: &Path,
    tracedecay_bin: Option<&str>,
) -> std::result::Result<bool, ()> {
    Ok(codex_source_manifest_matches_catalog_version(home)?
        && codex_plugin_enabled(home)?
        && codex_loaded_cache_matches_rendered_bundle(home, tracedecay_bin)?)
}

fn codex_plugin_enabled(home: &Path) -> std::result::Result<bool, ()> {
    let Some(marketplace_name) = codex_exact_personal_marketplace_name(home)? else {
        return Ok(false);
    };
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
    Ok(plugins
        .get(&format!("tracedecay@{marketplace_name}"))
        .and_then(|record| record.get("enabled"))
        .and_then(toml::Value::as_bool)
        == Some(true))
}

fn codex_loaded_cache_matches_rendered_bundle(
    home: &Path,
    tracedecay_bin: Option<&str>,
) -> std::result::Result<bool, ()> {
    let Some(marketplace_name) = codex_exact_personal_marketplace_name(home)? else {
        return Ok(false);
    };
    let cache_root = codex_plugin_cached_root(home, &marketplace_name).join(crate::PRODUCT_VERSION);
    let source_root = codex_plugin_install_dir(home);
    let (expected, relatives) = match tracedecay_bin {
        Some(tracedecay_bin) => {
            let rendered = rendered_global_plugin_files(tracedecay_bin).map_err(|_| ())?;
            let (digest, relatives) =
                super::rendered_bundle_content_digest(&rendered).map_err(|_| ())?;
            (Some(digest), relatives)
        }
        None => (
            None,
            codex_embedded_plugin_files()
                .into_iter()
                .map(|(relative, _)| relative.to_string())
                .collect(),
        ),
    };
    let Some(source) =
        super::observed_bundle_content_digest(&source_root, &relatives).map_err(|_| ())?
    else {
        return Ok(false);
    };
    let Some(cache) =
        super::observed_bundle_content_digest(&cache_root, &relatives).map_err(|_| ())?
    else {
        return Ok(false);
    };
    if source != cache || expected.is_some_and(|expected| source != expected) {
        return Ok(false);
    }
    let profile_root = crate::automation::skill_targets::profile_root_for_agent_home(home);
    let overlay = crate::automation::skill_targets::rendered_native_skill_overlay_files(
        &profile_root,
        crate::automation::skill_targets::SkillInstallTarget::Codex,
        &source_root,
    )
    .map_err(|_| ())?;
    let mut discovery_relatives = relatives;
    for (path, _) in overlay {
        let relative = path.strip_prefix(&source_root).map_err(|_| ())?;
        let relative = relative.to_str().ok_or(())?;
        discovery_relatives.push(relative.replace(std::path::MAIN_SEPARATOR, "/"));
    }
    super::observed_bundle_discovery_matches(
        &source_root,
        &cache_root,
        &discovery_relatives,
        &[".codex-plugin", "agents", "commands", "hooks", "skills"],
    )
    .map_err(|_| ())
}

fn codex_source_manifest_matches_catalog_version(home: &Path) -> std::result::Result<bool, ()> {
    codex_plugin_manifest_matches_catalog_version(&codex_plugin_manifest_path(home))
}

fn codex_plugin_manifest_matches_catalog_version(path: &Path) -> std::result::Result<bool, ()> {
    if !path.exists() {
        return Ok(false);
    }
    let manifest = load_json_file_strict(path).map_err(|_| ())?;
    Ok(
        manifest.get("name").and_then(serde_json::Value::as_str) == Some("tracedecay")
            && manifest.get("version").and_then(serde_json::Value::as_str)
                == Some(crate::PRODUCT_VERSION),
    )
}

fn codex_exact_personal_marketplace_name(home: &Path) -> std::result::Result<Option<String>, ()> {
    let path = codex_personal_marketplace_path(home);
    if !path.exists() {
        return Ok(None);
    }
    let marketplace = load_json_file_strict(&path).map_err(|_| ())?;
    let Some(name) = marketplace
        .get("name")
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.is_empty())
    else {
        return Ok(None);
    };
    validate_codex_marketplace_name(name).map_err(|_| ())?;
    let source_matches = marketplace
        .get("plugins")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|plugins| {
            plugins.iter().any(|entry| {
                entry.get("name").and_then(serde_json::Value::as_str) == Some("tracedecay")
                    && entry
                        .pointer("/source/source")
                        .and_then(serde_json::Value::as_str)
                        == Some("local")
                    && entry
                        .pointer("/source/path")
                        .and_then(serde_json::Value::as_str)
                        == Some(CODEX_GLOBAL_PLUGIN_SOURCE_PATH)
            })
        });
    Ok(source_matches.then(|| name.to_string()))
}

fn codex_plugin_is_natively_active(home: &Path, tracedecay_bin: Option<&str>) -> Result<bool> {
    codex_plugin_activation_state(home, tracedecay_bin).map_err(|()| TraceDecayError::Config {
        message: format!(
            "could not read Codex native plugin activation state at {}",
            codex_config_path(home).display()
        ),
    })
}

fn codex_non_interactive_install_state(
    home: &Path,
    tracedecay_bin: &str,
    staged_paths: Vec<PathBuf>,
) -> Result<NonInteractiveInstallOutcome> {
    if codex_plugin_is_natively_active(home, Some(tracedecay_bin))? {
        return Ok(NonInteractiveInstallOutcome::Ready);
    }
    let exact_marketplace_name =
        codex_exact_personal_marketplace_name(home).map_err(|()| TraceDecayError::Config {
            message: format!(
                "could not read Codex marketplace identity at {}",
                codex_personal_marketplace_path(home).display()
            ),
        })?;
    let marketplace_name = exact_marketplace_name
        .clone()
        .unwrap_or_else(|| codex_cached_marketplace_name(home));
    let exact_cache_present = exact_marketplace_name.is_some_and(|marketplace_name| {
        codex_plugin_cached_root(home, &marketplace_name)
            .join(crate::PRODUCT_VERSION)
            .join(".codex-plugin/plugin.json")
            .is_file()
    });
    if codex_plugin_enabled(home).map_err(|()| TraceDecayError::Config {
        message: format!(
            "could not read Codex native plugin activation state at {}",
            codex_config_path(home).display()
        ),
    })? && exact_cache_present
    {
        return Ok(NonInteractiveInstallOutcome::DeferredUserAction(
            DeferredUserAction {
                remediation: format!(
                    "Codex's loaded TraceDecay cache is stale. Run `codex plugin add tracedecay@{marketplace_name}` to reinstall it, re-trust changed hooks, then retry the TraceDecay lifecycle."
                ),
                staged_paths,
            },
        ));
    }
    Ok(NonInteractiveInstallOutcome::DeferredUserAction(
        DeferredUserAction {
            remediation: format!(
                "Codex activates plugins through its native cache. Run `codex plugin add tracedecay@{marketplace_name}` after TraceDecay stages the source package."
            ),
            staged_paths,
        },
    ))
}

fn codex_hook_state_table_is_explicit(contents: &str) -> bool {
    contents.lines().any(|line| line.trim() == "[hooks.state]")
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

fn remove_codex_retired_autodiscovered_files(install_dir: &Path) -> Result<()> {
    let managed = codex_plugin_managed_paths(install_dir)
        .into_iter()
        .collect::<HashSet<_>>();
    for relative_root in ["agents", "commands", "hooks", "skills"] {
        let root = install_dir.join(relative_root);
        let Ok(metadata) = std::fs::symlink_metadata(&root) else {
            continue;
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            continue;
        }
        let mut files =
            super::collect_regular_files(&root).map_err(|error| TraceDecayError::Config {
                message: format!(
                    "failed to inventory retired Codex plugin files under {}: {error}",
                    root.display()
                ),
            })?;
        files.sort();
        for file in files {
            if managed.contains(&file) {
                continue;
            }
            let Some(relative) = file
                .strip_prefix(install_dir)
                .ok()
                .and_then(Path::to_str)
                .map(|relative| relative.replace(std::path::MAIN_SEPARATOR, "/"))
            else {
                continue;
            };
            if !super::is_auto_discovered_entrypoint(&relative) {
                continue;
            }
            let Ok(contents) = std::fs::read(&file) else {
                continue;
            };
            if !retired_entrypoints::has_exact_identity(&relative, &contents) {
                continue;
            }
            super::safe_remove_host_file(&file).map_err(|error| TraceDecayError::Config {
                message: format!(
                    "failed to remove retired TraceDecay plugin file {}: {error}",
                    file.display()
                ),
            })?;
        }
        prune_empty_dirs(&root).map_err(|error| TraceDecayError::Config {
            message: format!(
                "failed to prune retired Codex plugin directories under {}: {error}",
                root.display()
            ),
        })?;
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
    let mut files =
        super::collect_regular_files(skills_dir).map_err(|e| TraceDecayError::Config {
            message: format!("failed to list {}: {e}", skills_dir.display()),
        })?;
    files.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for file in files {
        if managed.contains(&file) {
            super::safe_remove_host_file(&file).map_err(|e| TraceDecayError::Config {
                message: format!("failed to remove {}: {e}", file.display()),
            })?;
        }
    }
    prune_empty_dirs(skills_dir).map_err(|e| TraceDecayError::Config {
        message: format!("failed to prune empty Codex skill directories: {e}"),
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
    remove_codex_retired_autodiscovered_files(install_dir)?;
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
        CODEX_GLOBAL_PLUGIN_SOURCE_PATH,
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
                "Codex hook trust records in {} lack an explicit [hooks.state] table, so Codex still requests review; run `codex plugin add tracedecay@{marketplace_name}` to reinstall it, then use `/hooks` in Codex",
                config_path.display()
            )),
            CodexHookTrustState::Missing(missing) => dc.info(&format!(
                "Codex skips untrusted command hooks — missing trust for {} in {}; run `codex plugin add tracedecay@{marketplace_name}` to reinstall it, then use `/hooks` in Codex",
                missing.join(", "),
                config_path.display()
            )),
            CodexHookTrustState::Modified(modified) => dc.warn(&format!(
                "Codex hook trust is stale for {} in {} — the hook content changed since it was trusted, so Codex now skips it; run `codex plugin add tracedecay@{marketplace_name}` to reinstall it, then use `/hooks` in Codex",
                modified.join(", "),
                config_path.display()
            )),
        },
        Err(_) => dc.info(
            "Codex skips untrusted command hooks — run `codex plugin add tracedecay@{marketplace_name}` to reinstall it, then use `/hooks` in Codex to trust the tracedecay hooks",
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
