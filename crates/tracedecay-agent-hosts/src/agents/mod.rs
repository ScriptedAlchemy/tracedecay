//! Agent integration layer for CLI tools (Claude Code, `OpenCode`, Codex, etc.).
//!
//! Each supported agent implements the [`AgentIntegration`] trait for native
//! registration, health, and managed exports. Receipt-backed catalog
//! transactions own installation and removal.

pub mod antigravity;
mod bundle_identity;
pub mod claude;
pub mod cline;
pub mod codex;
pub mod context_scout;
pub mod copilot;
pub mod cursor;
pub(crate) mod cursor_diagnostics;
pub mod devin;
/// Legacy Cursor `serve` log marker; the root crate's `src/serve.rs`
/// re-exports this instead of declaring its own copy.
pub use cursor_diagnostics::DEGRADED_SERVE_STDERR_MARKER;
pub mod gemini;
mod git_post_commit_hook;
pub mod hermes;
pub mod host_bundle;
pub mod host_bundle_registry;
pub(crate) mod host_cli;
pub mod host_component_registration;
mod host_config_io;
pub mod kilo;
pub mod kimi;
pub mod kiro;
mod mcp_registration;
pub mod opencode;
pub mod plugin_bundle;
pub mod prompt_rules;
mod text_file_transaction;
pub(crate) use text_file_transaction::{
    TextFileMutation, update_config_file_transactionally, update_text_file_transactionally,
    update_two_config_files_transactionally,
};
pub(crate) mod retired_memory_digest;
pub mod roo_code;
pub mod vibe;
pub mod zed;

use std::path::{Path, PathBuf};

use tracedecay_automation_runtime::automation::host_io::ManagedSkillExportReport;
use tracedecay_automation_runtime::automation::skill_targets::SkillInstallSummary;
use tracedecay_domain::errors::Result;
use tracedecay_domain::errors::TraceDecayError;

pub use antigravity::AntigravityIntegration;
pub(crate) use bundle_identity::{
    is_auto_discovered_entrypoint, observed_bundle_content_digest,
    observed_bundle_discovery_matches, rendered_bundle_content_digest,
};
pub use claude::ClaudeIntegration;
pub use cline::ClineIntegration;
pub use codex::CodexIntegration;
pub use copilot::CopilotIntegration;
pub use cursor::CursorIntegration;
pub use devin::DevinIntegration;
pub use gemini::GeminiIntegration;
pub use hermes::HermesIntegration;
pub use kilo::KiloIntegration;
pub use kimi::KimiIntegration;
pub use kiro::KiroIntegration;
pub use opencode::OpenCodeIntegration;
pub use roo_code::RooCodeIntegration;
pub use vibe::VibeIntegration;
pub use zed::ZedIntegration;

pub use git_post_commit_hook::offer_git_post_commit_hook;
pub use host_config_io::{
    HostFileMetadataIdentityV1, JsonConfigDialect, backup_config_file, capture_host_file_metadata,
    config_backup_path, copilot_cli_dir, home_dir, host_config_write_intent_path, kiro_data_dir,
    load_json_file, load_json_file_strict, load_jsonc_file, load_jsonc_file_strict, load_toml_file,
    parse_jsonc, restore_config_backup, restore_host_file_metadata, safe_remove_host_file,
    safe_write_bytes_file, safe_write_bytes_file_with_metadata, safe_write_json_file,
    safe_write_text_file, vscode_data_dir, vscode_insiders_data_dir, which_tracedecay,
    which_tracedecay_path, with_host_config_write_intents, write_json_file, write_toml_file,
};
pub(crate) use host_config_io::{
    JsonConfigMutation, collect_regular_files, ensure_project_local_safe_path,
    ensure_project_local_safe_paths, hook_command, hook_command_for_platform, host_home_override,
    record_host_config_observation_bytes, sweep_superseded_plugin_siblings,
    update_json_config_transactionally, update_toml_config_transactionally,
};
// Host adapters under this module reach these through `super::` / `crate::agents::`.
#[cfg(test)]
use host_config_io::{
    TestHostConfigWritePauseController, pause_next_host_config_write_after_validation,
    pause_next_host_config_write_at_publication,
};
use host_config_io::{render_json_config, strip_jsonc_comments, which_tracedecay_path_from};
pub(crate) use mcp_registration::doctor_check_prompt_contains_tracedecay;
pub use mcp_registration::{
    McpDoctorLabels, McpUninstallPolicy, doctor_check_mcp_registration, expected_tool_perms,
    install_mcp_server_entry, mcp_config_has_tracedecay, mcp_registration_entry,
    mcp_servers_registration_state, read_only_tool_names, report_mcp_registration, tool_names,
    uninstall_mcp_server_entry,
};

#[hotpath::measure(label = "agent_hosts.agents.managed_skill.install_index")]
pub(crate) fn install_managed_skill_prompt_index(
    profile_home: &Path,
    prompt_path: &Path,
    target: tracedecay_automation_runtime::automation::skill_targets::SkillInstallTarget,
) -> Result<()> {
    let profile_root =
        tracedecay_automation_runtime::automation::skill_targets::profile_root_for_agent_home(
            profile_home,
        );
    retired_memory_digest::remove_state(&profile_root)?;
    retired_memory_digest::remove_prompt_block(prompt_path)?;
    tracedecay_automation_runtime::automation::skill_targets::install_managed_skills(
        &crate::host_io(),
        &profile_root,
        target,
        prompt_path,
    )?;
    Ok(())
}

#[hotpath::measure(label = "agent_hosts.agents.managed_skill.remove_index")]
pub(crate) fn remove_managed_skill_prompt_index(
    profile_home: &Path,
    prompt_path: &Path,
    target: tracedecay_automation_runtime::automation::skill_targets::SkillInstallTarget,
) -> Result<()> {
    let profile_root =
        tracedecay_automation_runtime::automation::skill_targets::profile_root_for_agent_home(
            profile_home,
        );
    retired_memory_digest::remove_state(&profile_root)?;
    tracedecay_automation_runtime::automation::skill_targets::remove_prompt_skill_index_for_target(
        &crate::host_io(),
        prompt_path,
        target,
    )?;
    retired_memory_digest::remove_prompt_block(prompt_path)
}

pub(crate) fn uses_default_user_profile(home: &Path, profile_root: &Path) -> bool {
    profile_root == home.join(".tracedecay")
}

/// Re-runs the managed-skill overlay/prompt-index export for every agent
/// integration that already has tracedecay installed under `home`, so a
/// lifecycle change (approve/disable/archive/restore) deploys without
/// waiting for the next catalog lifecycle or `update-plugin` pass.
///
/// Failures are collected per agent instead of aborting the sweep: a broken
/// export for one host must not block the others (or the lifecycle action
/// that triggered the refresh). Agents with no export destinations are
/// omitted from the result.
#[hotpath::measure(label = "agent_hosts.agents.managed_skill.export")]
pub fn export_managed_skills_to_agents(
    home: &Path,
    profile_root: &Path,
) -> Vec<ManagedSkillExportReport> {
    if !uses_default_user_profile(home, profile_root) {
        return Vec::new();
    }
    let mut reports = Vec::new();
    for ag in all_integrations() {
        match ag.export_managed_skills(home, profile_root) {
            Ok(exports) => {
                if !exports.is_empty() {
                    reports.push(ManagedSkillExportReport {
                        agent: ag.id().to_string(),
                        exports,
                        error: None,
                    });
                }
            }
            Err(err) => reports.push(ManagedSkillExportReport {
                agent: ag.id().to_string(),
                exports: Vec::new(),
                error: Some(err.to_string()),
            }),
        }
    }
    reports
}

/// Re-runs managed-skill exports for global installs under `home` plus
/// project-local installs under `project_root`. Reports are merged per agent
/// so dashboard callers can present one lifecycle refresh result per host.
#[hotpath::measure(label = "agent_hosts.agents.managed_skill.export_hosts")]
pub fn export_managed_skills_to_agent_hosts(
    home: &Path,
    project_root: &Path,
    profile_root: &Path,
) -> Vec<ManagedSkillExportReport> {
    if !uses_default_user_profile(home, profile_root) {
        return Vec::new();
    }
    let mut reports = Vec::new();
    for ag in all_integrations() {
        let mut exports = Vec::new();
        let mut errors = Vec::new();
        match ag.export_managed_skills(home, profile_root) {
            Ok(global_exports) => exports.extend(global_exports),
            Err(err) => errors.push(err.to_string()),
        }
        match ag.export_managed_skills_local(project_root, profile_root) {
            Ok(local_exports) => exports.extend(local_exports),
            Err(err) => errors.push(err.to_string()),
        }
        if !exports.is_empty() || !errors.is_empty() {
            reports.push(ManagedSkillExportReport {
                agent: ag.id().to_string(),
                exports,
                error: (!errors.is_empty()).then(|| errors.join("; ")),
            });
        }
    }
    reports
}

// ---------------------------------------------------------------------------
// AgentIntegration trait
// ---------------------------------------------------------------------------

/// A CLI agent that can be configured to use tracedecay via MCP.
pub trait AgentIntegration {
    /// Human-readable name (e.g. "Claude Code").
    fn name(&self) -> &'static str;

    /// CLI identifier used in `--agent <id>` (e.g. "claude").
    fn id(&self) -> &'static str;

    /// Returns true when this agent supports project-local configuration.
    fn supports_local_install(&self) -> bool {
        false
    }

    /// Validate non-interactive install readiness without changing host state.
    ///
    /// This is the read-only counterpart to
    /// [`AgentIntegration::prepare_non_interactive_install`]. Hosts that need
    /// manual activation report the same typed deferral without staging files.
    fn preflight_non_interactive_install(
        &self,
        _ctx: &InstallContext,
    ) -> Result<NonInteractiveInstallOutcome> {
        Ok(NonInteractiveInstallOutcome::Ready)
    }

    /// Prepare an install requested from a non-interactive orchestration path.
    ///
    /// Most integrations are immediately ready. Hosts whose official lifecycle
    /// requires user interaction may stage verified artifacts and return a
    /// typed deferral instead. Explicit install commands still surface that
    /// deferral as an error, while maintenance can warn and continue.
    fn prepare_non_interactive_install(
        &self,
        _ctx: &InstallContext,
    ) -> Result<NonInteractiveInstallOutcome> {
        Ok(NonInteractiveInstallOutcome::Ready)
    }

    /// Operator guidance for a host that activates deployed components only
    /// through an interactive UI, or `None` for a host TraceDecay can activate
    /// non-interactively.
    ///
    /// This is the read-only capability twin of the typed deferral
    /// [`AgentIntegration::prepare_non_interactive_install`] returns: doctor
    /// needs the same fact without an `InstallContext` and without staging
    /// anything. Every integration returning `Some` here must also return
    /// [`NonInteractiveInstallOutcome::DeferredUserAction`] from preflight —
    /// otherwise doctor would downgrade a state that an unattended reinstall
    /// could actually have repaired.
    fn interactive_activation_guidance(&self) -> Option<String> {
        None
    }

    /// Operator guidance for removing a host-native registration TraceDecay
    /// cannot drop itself, or `None` for a host whose registration the
    /// receipt-backed lifecycle owns outright.
    ///
    /// The removal twin of [`AgentIntegration::interactive_activation_guidance`].
    /// A host that activates only through an interactive UI also *deactivates*
    /// only there, so `Uninstall` must refuse while the registration stands —
    /// deleting the receipt-owned artifacts underneath a live registration
    /// leaves the host resolving a bundle that no longer exists. The refusal
    /// travels as [`host_bundle::HostBundleError::NativeRemovalRequired`],
    /// and this string is what makes it actionable: without it an operator is
    /// told a capability is unsupported rather than which host command to run.
    ///
    /// Every integration returning `Some` from `interactive_activation_guidance`
    /// should return `Some` here too; the two are the same host property seen
    /// from opposite ends of the lifecycle.
    fn interactive_removal_guidance(&self) -> Option<String> {
        None
    }

    /// Refresh tracedecay-generated artifacts (plugin code, baked binary
    /// paths, embedded assets) for every *detected* existing installation,
    /// without writing to any agent config file. Pins, MCP registrations,
    /// settings, and prompt rules are left byte-for-byte intact.
    ///
    /// The default reports [`UpdatePluginOutcome::ConfigOnly`]: most agents
    /// keep their entire tracedecay integration inside shared config files
    /// (MCP entries, hook blocks, prompt rules), so there is nothing to
    /// refresh that would not be a config write — `tracedecay reinstall`
    /// remains the path that reconciles those.
    fn update_plugin(&self, _ctx: &InstallContext) -> Result<UpdatePluginOutcome> {
        Ok(UpdatePluginOutcome::ConfigOnly)
    }

    /// Re-export the profile's active managed skills into every export
    /// destination this agent's existing installation owns (native overlay
    /// or prompt index), without touching any other config. Returns one
    /// summary per destination that was refreshed; the default returns an
    /// empty list for agents that either do not distribute managed skills
    /// or have no detected tracedecay installation under `home`.
    ///
    /// Implementors must never create a new installation here — only refresh
    /// artifacts already owned by a catalog receipt.
    fn export_managed_skills(
        &self,
        _home: &Path,
        _profile_root: &Path,
    ) -> Result<Vec<SkillInstallSummary>> {
        Ok(Vec::new())
    }

    /// Re-export active managed skills into receipt-owned destinations under a
    /// project/workspace. The default is a no-op for agents without
    /// project-local skill exports.
    fn export_managed_skills_local(
        &self,
        _project_root: &Path,
        _profile_root: &Path,
    ) -> Result<Vec<SkillInstallSummary>> {
        Ok(Vec::new())
    }

    /// Verify installation health (replaces agent-specific doctor checks).
    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext);

    /// Whether Doctor must report this supported host's absence even when it
    /// has no configuration directory yet. Most optional hosts stay quiet
    /// until their own registration exists; hosts with a documented deferred
    /// or native-only lifecycle opt in so Doctor does not turn their absence
    /// into an empty success.
    fn reports_absence_to_doctor(&self) -> bool {
        false
    }

    /// Evidence that the host application itself is present on this machine
    /// (its own config/profile surface exists), independent of whether
    /// tracedecay is integrated into it. Doctor uses this to warn uniformly
    /// about detected-but-unintegrated hosts instead of printing nothing for
    /// them. The default is `None`: a host without a cheap, reliable presence
    /// probe stays quiet rather than guessing at foreign config layouts.
    fn detected_host_surface(&self, _home: &Path) -> Option<PathBuf> {
        None
    }

    /// Verify installation health using the daemon-owned snapshot already
    /// collected by Doctor. Integrations with daemon-backed diagnostics can
    /// override this without issuing another daemon call.
    fn healthcheck_with_daemon_status(
        &self,
        dc: &mut DoctorCounters,
        ctx: &HealthcheckContext,
        _daemon_status: Option<&serde_json::Value>,
    ) {
        self.healthcheck(dc, ctx);
    }

    /// Read-only native registration state for one receipt-backed component.
    /// Doctor calls this only for components enumerated from lifecycle
    /// receipts; implementations must not infer uninstalled catalog pairs.
    fn host_component_registration(
        &self,
        _component: host_bundle::HostBundleComponentV1,
        _ctx: &HealthcheckContext,
    ) -> host_bundle::HostBundleRegistrationStateV1 {
        host_bundle::HostBundleRegistrationStateV1::Missing
    }

    /// Registration state for a concrete lifecycle policy. Most hosts ignore
    /// install policy; Hermes uses it to distinguish dashboard-enabled and
    /// dashboard-disabled registrations without weakening doctor readback.
    fn host_component_registration_for_lifecycle(
        &self,
        component: host_bundle::HostBundleComponentV1,
        health: &HealthcheckContext,
        _install: &InstallContext,
    ) -> host_bundle::HostBundleRegistrationStateV1 {
        self.host_component_registration(component, health)
    }

    /// Returns true if this agent appears to be installed on the system
    /// (its config directory exists).
    fn is_detected(&self, _home: &Path) -> bool {
        false
    }

    /// Returns true if tracedecay MCP server is already registered in this
    /// agent's config. Used for migration backfill.
    fn has_tracedecay(&self, _home: &Path) -> bool {
        false
    }

    /// The primary native config file this agent's catalog registration
    /// projection owns, if any. Returning `Some(path)` lets tests and lifecycle tools
    /// ask the integration for its own path instead of re-deriving it via
    /// `#[cfg(target_os = ...)]`, which is how the v4.3.15 zed regression
    /// test silently disagreed with the Windows install path. Implementors
    /// should return the same path the install helper writes to, including
    /// any platform-conditional branching. Returning `None` means "no single
    /// primary config" (e.g. an append-only TOML file with no rewrite path).
    fn primary_config_path(&self, _home: &Path) -> Option<PathBuf> {
        None
    }

    /// Every mutable host registration/configuration path participating in an
    /// aggregate component-set lifecycle. The transaction stages backups for
    /// all returned paths before invoking the host registration authority.
    fn host_registration_paths(&self, home: &Path) -> Vec<PathBuf> {
        self.primary_config_path(home).into_iter().collect()
    }

    /// Mutable registration paths for the exact selected components. Hosts
    /// with disjoint component ownership override this so a companion
    /// transaction never snapshots or restores another component's state.
    fn host_component_registration_paths(
        &self,
        _components: &[host_bundle::HostBundleComponentV1],
        home: &Path,
    ) -> Vec<PathBuf> {
        self.host_registration_paths(home)
    }

    /// Fallible exact registration inventory used by the transaction backup.
    ///
    /// Hosts whose paths depend on validated profile data override this rather
    /// than silently dropping files from rollback ownership.
    fn host_component_registration_paths_checked(
        &self,
        components: &[host_bundle::HostBundleComponentV1],
        home: &Path,
    ) -> Result<Vec<PathBuf>> {
        Ok(self.host_component_registration_paths(components, home))
    }

    /// Exact project-scoped paths the catalog registration projection may
    /// create, replace, or remove. The aggregate transaction snapshots this
    /// complete set before invoking the projection.
    fn project_host_component_registration_paths(
        &self,
        _components: &[host_bundle::HostBundleComponentV1],
        _home: &Path,
        _project_path: &Path,
    ) -> Result<Vec<PathBuf>> {
        Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: format!(
                "{} has no catalog-backed project registration projection",
                self.name()
            ),
        })
    }

    /// Re-activate host-native registration for already-deployed component
    /// assets without rendering or copying those assets again.
    fn activate_deployed_host_registration(&self, _ctx: &InstallContext) -> Result<()> {
        Ok(())
    }

    /// Re-activate only the native registration owned by the selected
    /// receipt-backed components. The default forwards Core to the host's
    /// deployed registration boundary.
    fn activate_deployed_host_component_registration(
        &self,
        components: &[host_bundle::HostBundleComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        if components.contains(&host_bundle::HostBundleComponentV1::Core) {
            self.activate_deployed_host_registration(ctx)
        } else {
            Ok(())
        }
    }

    /// Remove host-native registration for component assets already removed
    /// by the receipt-backed lifecycle without deleting or rewriting any
    /// deployed component artifacts.
    fn deactivate_deployed_host_registration(&self, _ctx: &InstallContext) -> Result<()> {
        Ok(())
    }

    /// Remove only the native registration owned by the selected
    /// receipt-backed components.
    fn deactivate_deployed_host_component_registration(
        &self,
        components: &[host_bundle::HostBundleComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        if components.contains(&host_bundle::HostBundleComponentV1::Core) {
            self.deactivate_deployed_host_registration(ctx)
        } else {
            Ok(())
        }
    }

    /// Apply only this host's project-scoped registration projection.
    ///
    /// The component-set transaction calls this boundary after it has staged
    /// exact registration backups. Implementations must mutate only bounded
    /// project registration paths; they must not install global assets.
    fn activate_project_host_component_registration(
        &self,
        _components: &[host_bundle::HostBundleComponentV1],
        _ctx: &InstallContext,
        _project_path: &Path,
    ) -> Result<()> {
        Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: format!(
                "{} has no catalog-backed project registration projection",
                self.name()
            ),
        })
    }

    /// Remove only this host's project-scoped registration projection.
    fn deactivate_project_host_component_registration(
        &self,
        _components: &[host_bundle::HostBundleComponentV1],
        _ctx: &InstallContext,
        _project_path: &Path,
    ) -> Result<()> {
        Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: format!(
                "{} has no catalog-backed project registration projection",
                self.name()
            ),
        })
    }
}

/// User action required to finish a lifecycle operation that `TraceDecay` cannot
/// safely perform through a non-interactive host API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeferredUserAction {
    /// Exact operator-facing remediation.
    pub remediation: String,
    /// Verified artifacts staged for the user to apply through the host.
    pub staged_paths: Vec<PathBuf>,
}

/// Result of preparing an install for a non-interactive caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NonInteractiveInstallOutcome {
    /// The caller may continue through the ordinary install path.
    Ready,
    /// Verified work was staged, but the host requires explicit user action.
    DeferredUserAction(DeferredUserAction),
}

/// Outcome of [`AgentIntegration::update_plugin`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdatePluginOutcome {
    /// Generated artifacts were refreshed at these locations.
    Refreshed(Vec<PathBuf>),
    /// The integration ships generated artifacts, but none were detected on
    /// this machine — nothing was written.
    NotInstalled,
    /// The integration only writes shared config files; there are no
    /// tracedecay-generated artifacts to refresh without touching config.
    ConfigOnly,
    /// Verified artifacts were staged, but the host requires explicit user
    /// action before it can activate them.
    DeferredUserAction(DeferredUserAction),
}

/// Context passed to catalog-backed host registration and refresh operations.
pub struct InstallContext {
    pub home: PathBuf,
    pub tracedecay_bin: String,
    pub tool_permissions: Vec<String>,
    /// Codex update/uninstall can use this as an explicit repo-local plugin
    /// target. Other integrations ignore it.
    pub project_root: Option<PathBuf>,
    /// Hermes only: deploy the dashboard wrapper plugin page alongside the
    /// agent plugin (default; `tracedecay install --agent hermes
    /// --no-dashboard` opts out and removes a previous deploy). Other agents
    /// ignore this field.
    pub dashboard: bool,
}

/// Context passed to [`AgentIntegration::healthcheck`].
pub struct HealthcheckContext {
    pub home: PathBuf,
    pub project_path: PathBuf,
}

/// Where an MCP server registration is being written.
///
/// Replaces the previous `(is_local_install, enable_global_db)` boolean pair
/// in the per-agent `install_mcp_server` helpers, which only ever took two of
/// the four combinations. Encoding the intent as an enum makes the two invalid
/// combinations unrepresentable and lets each agent map the scope to its own
/// args/env wiring via an exhaustive `match`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstallScope {
    /// User-global install: `serve` without an explicit project path.
    Global,
    /// Project-local install: `serve --path .` with an explicit project route.
    ProjectLocal,
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// Returns the agent matching `id`, or an error if unknown.
pub fn get_integration(id: &str) -> Result<Box<dyn AgentIntegration>> {
    match id {
        "claude" => Ok(Box::new(ClaudeIntegration)),
        "opencode" => Ok(Box::new(OpenCodeIntegration)),
        "codex" => Ok(Box::new(CodexIntegration)),
        "gemini" => Ok(Box::new(GeminiIntegration)),
        "copilot" => Ok(Box::new(CopilotIntegration)),
        "cursor" => Ok(Box::new(CursorIntegration)),
        "devin" => Ok(Box::new(DevinIntegration)),
        "hermes" => Ok(Box::new(HermesIntegration)),
        "zed" => Ok(Box::new(ZedIntegration)),
        "cline" => Ok(Box::new(ClineIntegration)),
        "roo-code" => Ok(Box::new(RooCodeIntegration)),
        "antigravity" => Ok(Box::new(AntigravityIntegration)),
        "kilo" => Ok(Box::new(KiloIntegration)),
        "kiro" => Ok(Box::new(KiroIntegration)),
        "kimi" => Ok(Box::new(KimiIntegration)),
        "vibe" => Ok(Box::new(VibeIntegration)),
        _ => Err(TraceDecayError::Config {
            message: format!(
                "unknown agent: \"{id}\". Available agents: {}",
                available_integrations().join(", ")
            ),
        }),
    }
}

/// Returns all registered agents.
pub fn all_integrations() -> Vec<Box<dyn AgentIntegration>> {
    vec![
        Box::new(ClaudeIntegration),
        Box::new(OpenCodeIntegration),
        Box::new(CodexIntegration),
        Box::new(GeminiIntegration),
        Box::new(CopilotIntegration),
        Box::new(CursorIntegration),
        Box::new(DevinIntegration),
        Box::new(HermesIntegration),
        Box::new(ZedIntegration),
        Box::new(ClineIntegration),
        Box::new(RooCodeIntegration),
        Box::new(AntigravityIntegration),
        Box::new(KiloIntegration),
        Box::new(KiroIntegration),
        Box::new(KimiIntegration),
        Box::new(VibeIntegration),
    ]
}

/// Returns the CLI identifiers of all registered agents (for help text).
pub fn available_integrations() -> Vec<&'static str> {
    vec![
        "claude",
        "opencode",
        "codex",
        "gemini",
        "copilot",
        "cursor",
        "devin",
        "hermes",
        "zed",
        "cline",
        "roo-code",
        "antigravity",
        "kilo",
        "kiro",
        "kimi",
        "vibe",
    ]
}

#[cfg(test)]
#[test]
fn devin_is_a_registered_independent_agent() {
    let integration = get_integration("devin").expect("Devin integration is registered");
    assert_eq!(integration.name(), "Devin");
    assert!(available_integrations().contains(&"devin"));
}

pub fn integration_id_for_host(host: host_bundle::HostKindV1) -> &'static str {
    match host {
        host_bundle::HostKindV1::ClaudeCode => "claude",
        host_bundle::HostKindV1::CursorDesktop | host_bundle::HostKindV1::CursorCloud => "cursor",
        host_bundle::HostKindV1::Codex => "codex",
        host_bundle::HostKindV1::Devin => "devin",
        host_bundle::HostKindV1::Zed => "zed",
        host_bundle::HostKindV1::Antigravity => "antigravity",
        host_bundle::HostKindV1::Vibe => "vibe",
        host_bundle::HostKindV1::Hermes => "hermes",
        host_bundle::HostKindV1::Kiro => "kiro",
        host_bundle::HostKindV1::ClineFamily => "cline",
        host_bundle::HostKindV1::Cline => "cline",
        host_bundle::HostKindV1::RooCode => "roo-code",
        host_bundle::HostKindV1::Kilo => "kilo",
        host_bundle::HostKindV1::KimiCode => "kimi",
        host_bundle::HostKindV1::OpenCode => "opencode",
        host_bundle::HostKindV1::Gemini => "gemini",
        host_bundle::HostKindV1::Copilot => "copilot",
    }
}

/// 40-hex generator-commit fixture for this crate's unit tests. Production
/// callers pass the registered product runtime's commit SHA instead.
#[cfg(test)]
pub(crate) const TEST_GENERATOR_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

struct AgentRegistrationInspector<'a> {
    context: &'a HealthcheckContext,
}

impl host_bundle::HostBundleRegistrationInspectorV1 for AgentRegistrationInspector<'_> {
    fn inspect_registration(
        &self,
        host: host_bundle::HostKindV1,
        component: host_bundle::HostBundleComponentV1,
    ) -> host_bundle::HostBundleRegistrationStateV1 {
        get_integration(integration_id_for_host(host)).map_or(
            host_bundle::HostBundleRegistrationStateV1::Missing,
            |integration| integration.host_component_registration(component, self.context),
        )
    }

    fn interactive_activation_guidance(&self, host: host_bundle::HostKindV1) -> Option<String> {
        get_integration(integration_id_for_host(host))
            .ok()
            .and_then(|integration| integration.interactive_activation_guidance())
    }
}

#[hotpath::measure(label = "agent_hosts.agents.host_bundle.inspect")]
pub fn inspect_receipt_backed_host_components(
    context: &HealthcheckContext,
    lifecycle_root: &Path,
    generator_commit: &str,
) -> std::result::Result<host_bundle::HostBundleDoctorReportV1, host_bundle::HostBundleError> {
    host_bundle::inspect_installed_host_bundle_components_at(
        &context.home,
        lifecycle_root,
        &AgentRegistrationInspector { context },
        generator_commit,
    )
}

// ---------------------------------------------------------------------------
// DoctorCounters
// ---------------------------------------------------------------------------

/// Diagnostic counters for doctor checks.
#[derive(Default)]
pub struct DoctorCounters {
    pub issues: u32,
    pub warnings: u32,
}

impl DoctorCounters {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn pass(&self, msg: &str) {
        eprintln!("  \x1b[32m✔\x1b[0m {msg}");
    }
    pub fn fail(&mut self, msg: &str) {
        eprintln!("  \x1b[31m✘\x1b[0m {msg}");
        self.issues += 1;
    }
    pub fn warn(&mut self, msg: &str) {
        eprintln!("  \x1b[33m!\x1b[0m {msg}");
        self.warnings += 1;
    }
    pub fn info(&self, msg: &str) {
        eprintln!("    {msg}");
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

#[macro_export]
macro_rules! cli_fallback_args_invocation_lit {
    () => {
        "`tracedecay tool <name> --args '<json>'` — the same JSON arguments object as the MCP tool; \
pipe it via `--args -` (a quoted heredoc) when it contains quotes or newlines"
    };
}

/// CLI-fallback steering paragraph shared by every host's prompt rules.
///
/// Mirrors the guidance in the MCP server instructions and the bundled
/// `using-the-cli` skill: when the MCP transport fails, agents should fall
/// back to the `tracedecay tool` CLI instead of abandoning tracedecay or
/// poking at `.tracedecay` databases directly.
pub const CLI_FALLBACK_PROMPT_RULES: &str = concat!(
    "If a tracedecay MCP call errors, times out, \
or the server is disconnected, every tool is also available as a shell command: ",
    cli_fallback_args_invocation_lit!(),
    " \
(`tracedecay tool` lists all tools, `tracedecay tool <name> --help` shows parameters). \
Pass schema fields inside the JSON object; never invent per-key flags or enum values from memory. \
Fall back to that CLI instead of querying `.tracedecay` databases directly or abandoning tracedecay."
);

/// True when a `SKILL.md` carries a TraceDecay authorship marker. Retired
/// plugin artifacts use this narrow check so same-name user workflows remain
/// outside TraceDecay's cleanup authority.
pub(crate) fn skill_contents_have_tracedecay_marker(contents: &str) -> bool {
    contents.lines().map(str::trim).any(|line| {
        line.starts_with("name: tracedecay:")
            || line.starts_with("description: TraceDecay ")
            || line.contains("TraceDecay MCP")
            || line.contains("tracedecay_")
            || line.contains("`tracedecay:")
    })
}

/// Interactively pick which agents to install/uninstall.
///
/// - 0 detected agents → returns an error.
/// - 1 detected and not already installed → returns it directly (no prompt).
/// - Otherwise → asks a Y/n question for each detected agent.
///
/// Returns `(to_install, to_uninstall)`.
pub fn pick_integrations_interactive(
    home: &Path,
    installed: &[String],
) -> Result<(Vec<String>, Vec<String>)> {
    let detected: Vec<Box<dyn AgentIntegration>> = all_integrations()
        .into_iter()
        .filter(|ag| ag.is_detected(home))
        .collect();

    if detected.is_empty() {
        return Err(TraceDecayError::Config {
            message: "No supported agents detected on this system".to_string(),
        });
    }

    // Fast path: exactly one detected agent and it isn't installed yet.
    if detected.len() == 1 && !installed.contains(&detected[0].id().to_string()) {
        let id = detected[0].id().to_string();
        return Ok((vec![id], vec![]));
    }

    let mut to_install = Vec::new();
    let mut to_uninstall = Vec::new();

    for ag in &detected {
        let id = ag.id().to_string();
        let already = installed.contains(&id);
        if already {
            eprint!("Keep TraceDecay for {}? [Y/n] ", ag.name());
        } else {
            eprint!("Install TraceDecay for {}? [Y/n] ", ag.name());
        }

        let mut input = String::new();
        std::io::stdin()
            .read_line(&mut input)
            .map_err(|e| TraceDecayError::Config {
                message: format!("failed to read input: {e}"),
            })?;
        let answer = input.trim().to_lowercase();
        let yes = answer.is_empty() || answer == "y" || answer == "yes";

        if yes && !already {
            to_install.push(id);
        } else if !yes && already {
            to_uninstall.push(id);
        }
    }

    Ok((to_install, to_uninstall))
}
