// Rust guideline compliant 2025-10-17
//! Agent integration layer for CLI tools (Claude Code, `OpenCode`, Codex, etc.).
//!
//! Each supported agent implements the [`AgentIntegration`] trait which provides
//! `install`, `uninstall`, and `healthcheck` operations. The MCP server
//! itself is agent-agnostic; this module handles the per-agent config
//! plumbing (registering the MCP server, permissions, hooks, prompt rules).

pub mod antigravity;
pub mod claude;
pub mod cline;
pub mod codex;
pub mod context_scout_model;
pub mod context_scout_owner;
pub mod context_scout_ports;
pub mod context_scout_v2;
pub mod copilot;
pub mod cursor;
pub(crate) mod cursor_diagnostics;
pub mod gemini;
pub mod hermes;
pub mod host_bundle_registry;
pub mod host_bundle_v2;
pub mod host_component_registration;
pub mod kilo;
pub mod kimi;
pub mod kiro;
pub mod opencode;
pub mod plugin_bundle;
pub mod prompt_rules;
pub mod roo_code;
pub mod vibe;
pub mod zed;

use std::cell::RefCell;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::automation::skill_targets::SkillInstallSummary;
use crate::errors::Result;
use crate::errors::TraceDecayError;
use crate::ports::mcp_tools::advertised_tools;

pub use antigravity::AntigravityIntegration;
pub use claude::ClaudeIntegration;
pub use cline::ClineIntegration;
pub use codex::CodexIntegration;
pub use copilot::CopilotIntegration;
pub use cursor::CursorIntegration;
pub use gemini::GeminiIntegration;
pub use hermes::HermesIntegration;
pub use kilo::KiloIntegration;
pub use kimi::KimiIntegration;
pub use kiro::KiroIntegration;
pub use opencode::OpenCodeIntegration;
pub use roo_code::RooCodeIntegration;
pub use vibe::VibeIntegration;
pub use zed::ZedIntegration;

pub(crate) fn install_managed_skill_prompt_index(
    profile_home: &Path,
    prompt_path: &Path,
    target: crate::automation::skill_targets::SkillInstallTarget,
) -> Result<()> {
    let profile_root = crate::automation::skill_targets::profile_root_for_agent_home(profile_home);
    crate::automation::skill_targets::install_managed_skills(&profile_root, target, prompt_path)?;
    crate::automation::memory_digest::sync_memory_digest_export(
        &profile_root,
        target,
        prompt_path,
    )?;
    Ok(())
}

pub(crate) fn remove_managed_skill_prompt_index(
    profile_home: &Path,
    prompt_path: &Path,
    target: crate::automation::skill_targets::SkillInstallTarget,
) -> Result<()> {
    let profile_root = crate::automation::skill_targets::profile_root_for_agent_home(profile_home);
    crate::automation::skill_targets::remove_prompt_skill_index_for_target(prompt_path, target)?;
    crate::automation::memory_digest::remove_memory_digest_export(
        &profile_root,
        target,
        prompt_path,
    )
}

/// Per-agent outcome of a managed-skill export refresh, keyed by agent id.
/// `error` carries the failure message when the refresh failed; `exports`
/// lists the destinations that were (re)written on success.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ManagedSkillExportReport {
    pub agent: String,
    pub exports: Vec<SkillInstallSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub(crate) fn uses_default_user_profile(home: &Path, profile_root: &Path) -> bool {
    profile_root == home.join(".tracedecay")
}

/// Re-runs the managed-skill overlay/prompt-index export for every agent
/// integration that already has tracedecay installed under `home`, so a
/// lifecycle change (approve/disable/archive/restore) deploys without
/// waiting for the next `tracedecay install` / `update-plugin`.
///
/// Failures are collected per agent instead of aborting the sweep: a broken
/// export for one host must not block the others (or the lifecycle action
/// that triggered the refresh). Agents with no export destinations are
/// omitted from the result.
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

    /// Register MCP server, permissions, hooks, and prompt rules.
    fn install(&self, ctx: &InstallContext) -> Result<()>;

    /// Returns true when this agent supports project-local configuration.
    fn supports_local_install(&self) -> bool {
        false
    }

    /// Register MCP server, permissions, hooks, and prompt rules under a
    /// project/workspace directory instead of the user's global config.
    fn install_local(&self, _ctx: &InstallContext, _project_path: &Path) -> Result<()> {
        Err(TraceDecayError::Config {
            message: format!(
                "{} does not support `tracedecay install --local` yet. \
                 Run `tracedecay install --agent {}` for a global install.",
                self.name(),
                self.id()
            ),
        })
    }

    /// Remove only the project-local registration and generated assets owned
    /// by [`AgentIntegration::install_local`].
    fn uninstall_local(&self, _ctx: &InstallContext, _project_path: &Path) -> Result<()> {
        Err(TraceDecayError::Config {
            message: format!("{} does not support project-local uninstall", self.name()),
        })
    }

    /// Optional hook run after a successful [`AgentIntegration::install`] or
    /// [`AgentIntegration::install_local`]. The default is a no-op.
    ///
    /// Agents that need to react to their own installation override this — for
    /// example, Cursor registers the project's current git branch for
    /// tracedecay indexing. Keeping per-agent post-install behavior behind the
    /// trait means the `install` / `reinstall` command flow never has to
    /// special-case individual agents by id.
    fn post_install<'a>(
        &'a self,
        _project_path: Option<&'a Path>,
    ) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
        Box::pin(std::future::ready(()))
    }

    /// Validate non-interactive install readiness without changing host state.
    ///
    /// This is the pre-migration counterpart to
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
    /// Implementors must never create a new installation here — only
    /// refresh artifacts that `install` already wrote.
    fn export_managed_skills(
        &self,
        _home: &Path,
        _profile_root: &Path,
    ) -> Result<Vec<SkillInstallSummary>> {
        Ok(Vec::new())
    }

    /// Re-export active managed skills into destinations created by
    /// [`AgentIntegration::install_local`] under a project/workspace. The
    /// default is a no-op for agents without project-local skill exports.
    fn export_managed_skills_local(
        &self,
        _project_root: &Path,
        _profile_root: &Path,
    ) -> Result<Vec<SkillInstallSummary>> {
        Ok(Vec::new())
    }

    /// Remove everything installed by [`AgentIntegration::install`].
    fn uninstall(&self, ctx: &InstallContext) -> Result<()>;

    /// Verify installation health (replaces agent-specific doctor checks).
    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext);

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
        _component: host_bundle_v2::HostBundleComponentV1,
        _ctx: &HealthcheckContext,
    ) -> host_bundle_v2::HostBundleRegistrationStateV1 {
        host_bundle_v2::HostBundleRegistrationStateV1::Missing
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

    /// The single config file this agent rewrites on install / uninstall, if
    /// any. Returning `Some(path)` lets tests (and any future external tool)
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
    /// all returned paths before invoking the host registration adapter.
    fn host_registration_paths(&self, home: &Path) -> Vec<PathBuf> {
        self.primary_config_path(home).into_iter().collect()
    }

    /// Mutable registration paths for the exact selected components. Hosts
    /// with disjoint component ownership override this so a companion
    /// transaction never snapshots or restores another component's state.
    fn host_component_registration_paths(
        &self,
        _components: &[host_bundle_v2::HostBundleComponentV1],
        home: &Path,
    ) -> Vec<PathBuf> {
        self.host_registration_paths(home)
    }

    fn host_component_registration_paths_at(
        &self,
        components: &[host_bundle_v2::HostBundleComponentV1],
        home: &Path,
        _project_path: &Path,
    ) -> Vec<PathBuf> {
        self.host_component_registration_paths(components, home)
    }

    /// Re-activate host-native registration for already-deployed component
    /// assets without rendering or copying those assets again.
    fn activate_deployed_host_registration(&self, _ctx: &InstallContext) -> Result<()> {
        Ok(())
    }

    /// Re-activate only the native registration owned by the selected
    /// receipt-backed components. The default keeps compatibility for hosts
    /// whose only registered component is Core.
    fn activate_deployed_host_component_registration(
        &self,
        components: &[host_bundle_v2::HostBundleComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        if components.contains(&host_bundle_v2::HostBundleComponentV1::Core) {
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
        components: &[host_bundle_v2::HostBundleComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        if components.contains(&host_bundle_v2::HostBundleComponentV1::Core) {
            self.deactivate_deployed_host_registration(ctx)
        } else {
            Ok(())
        }
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

/// Context passed to [`AgentIntegration::install`] and [`AgentIntegration::uninstall`].
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

pub fn integration_id_for_host(host: host_bundle_v2::HostKindV1) -> &'static str {
    match host {
        host_bundle_v2::HostKindV1::ClaudeCode => "claude",
        host_bundle_v2::HostKindV1::CursorDesktop | host_bundle_v2::HostKindV1::CursorCloud => {
            "cursor"
        }
        host_bundle_v2::HostKindV1::Codex => "codex",
        host_bundle_v2::HostKindV1::Hermes => "hermes",
        host_bundle_v2::HostKindV1::Kiro => "kiro",
        host_bundle_v2::HostKindV1::ClineFamily => "cline",
        host_bundle_v2::HostKindV1::Cline => "cline",
        host_bundle_v2::HostKindV1::RooCode => "roo-code",
        host_bundle_v2::HostKindV1::Kilo => "kilo",
        host_bundle_v2::HostKindV1::KimiCode => "kimi",
        host_bundle_v2::HostKindV1::OpenCode => "opencode",
    }
}

struct AgentRegistrationInspector<'a> {
    context: &'a HealthcheckContext,
}

impl host_bundle_v2::HostBundleRegistrationInspectorV1 for AgentRegistrationInspector<'_> {
    fn inspect_registration(
        &self,
        host: host_bundle_v2::HostKindV1,
        component: host_bundle_v2::HostBundleComponentV1,
    ) -> host_bundle_v2::HostBundleRegistrationStateV1 {
        get_integration(integration_id_for_host(host)).map_or(
            host_bundle_v2::HostBundleRegistrationStateV1::Missing,
            |integration| integration.host_component_registration(component, self.context),
        )
    }
}

pub fn inspect_receipt_backed_host_components(
    context: &HealthcheckContext,
    lifecycle_root: &Path,
) -> std::result::Result<host_bundle_v2::HostBundleDoctorReportV1, host_bundle_v2::HostBundleError>
{
    host_bundle_v2::inspect_installed_host_bundle_components_at(
        &context.home,
        lifecycle_root,
        &AgentRegistrationInspector { context },
    )
}

pub fn inspect_installed_host_components(
    context: &HealthcheckContext,
) -> Result<host_bundle_v2::HostBundleDoctorReportV1> {
    let lifecycle_root = host_bundle_v2::resolved_host_bundle_lifecycle_root()?;
    inspect_receipt_backed_host_components(context, &lifecycle_root).map_err(|error| {
        TraceDecayError::Config {
            message: format!(
                "could not inspect receipt-backed host components in {}: {error}",
                lifecycle_root.display()
            ),
        }
    })
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

/// Load a JSON file, returning an empty object on missing/invalid.
/// Use this for **read-only** paths (healthcheck, `has_tracedecay`, etc.).
/// For install/edit paths, use [`load_json_file_strict`] instead.
pub fn load_json_file(path: &Path) -> serde_json::Value {
    if path.exists() {
        let contents = std::fs::read_to_string(path).unwrap_or_default();
        serde_json::from_str(&contents).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    }
}

/// Load a JSON file for **editing**. Unlike [`load_json_file`], this returns
/// an error if the file exists but cannot be parsed, preventing silent data
/// loss when the modified value is written back.
///
/// # Error conditions
/// - File exists but is not readable (permissions, I/O error).
/// - File exists and has content but contains invalid JSON.
///
/// Returns `Ok(json!({}))` only when the file does not exist or is empty,
/// which is safe for creating a new config from scratch.
pub fn load_json_file_strict(path: &Path) -> Result<serde_json::Value> {
    if !path.exists() {
        return Ok(serde_json::json!({}));
    }
    let contents = std::fs::read_to_string(path).map_err(|e| TraceDecayError::Config {
        message: format!("cannot read {}: {e}", path.display()),
    })?;
    if contents.trim().is_empty() {
        return Ok(serde_json::json!({}));
    }
    serde_json::from_str(&contents).map_err(|e| TraceDecayError::Config {
        message: format!(
            "cannot parse {} as JSON: {e}\n  \
             Hint: fix the JSON syntax manually and re-run the command,\n  \
             or delete the file to start fresh",
            path.display()
        ),
    })
}

/// Create a backup copy of a config file before modifying it.
///
/// The backup itself is written atomically: content is first written to a
/// staging file (`.bak.new`), then renamed to `.bak`. This ensures the
/// `.bak` file is never half-written even if the process is killed.
///
/// Returns `Ok(Some(backup_path))` when a backup was created, or `Ok(None)`
/// when the file did not exist (nothing to back up).
///
/// # Error conditions
/// - File exists but cannot be read (permissions, I/O error).
/// - Staging file cannot be written (disk full, permissions).
/// - Staging file cannot be renamed to `.bak` (cross-device, permissions).
pub fn config_backup_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.bak", path.display()))
}

pub fn backup_config_file(path: &Path) -> Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let backup_path = config_backup_path(path);
    let staging_path = PathBuf::from(format!("{}.bak.new", path.display()));

    // Read original content
    let content = std::fs::read(path).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to read {} for backup: {e}\n  \
             Hint: check file permissions",
            path.display()
        ),
    })?;
    // Write to staging file
    std::fs::write(&staging_path, &content).map_err(|e| {
        std::fs::remove_file(&staging_path).ok();
        TraceDecayError::Config {
            message: format!(
                "failed to write backup staging file {}: {e}\n  \
                 Hint: check available disk space and permissions",
                staging_path.display()
            ),
        }
    })?;
    let backup_metadata =
        capture_host_file_metadata(&staging_path).map_err(|error| TraceDecayError::Config {
            message: format!(
                "failed to inspect backup staging file {}: {error}",
                staging_path.display()
            ),
        })?;
    persist_host_config_write_intent(&backup_path, &content, Some(&backup_metadata))?;

    // Atomic rename staging → .bak
    std::fs::rename(&staging_path, &backup_path).map_err(|e| {
        std::fs::remove_file(&staging_path).ok();
        TraceDecayError::Config {
            message: format!(
                "failed to create backup {}: {e}\n  \
                 Hint: check file permissions",
                backup_path.display()
            ),
        }
    })?;

    Ok(Some(backup_path))
}

/// Restore a config file from its backup. Prints instructions for manual
/// recovery if the restore itself fails.
pub fn restore_config_backup(original: &Path, backup: &Path) {
    match std::fs::copy(backup, original) {
        Ok(_) => {
            eprintln!(
                "\x1b[33m⚠\x1b[0m  Restored {} from backup",
                original.display()
            );
        }
        Err(e) => {
            eprintln!(
                "\x1b[31m✗\x1b[0m Failed to auto-restore {} from backup: {e}",
                original.display()
            );
            eprintln!(
                "  Manual recovery: cp '{}' '{}'",
                backup.display(),
                original.display()
            );
        }
    }
}

/// Write a JSON value to a file via atomic rename.
///
/// The caller is responsible for creating the backup via
/// [`backup_config_file`] before loading the config. Pass the backup path
/// here so that it can be mentioned in error messages and used for restore
/// if the rename somehow leaves the target in a bad state.
///
/// # Strategy
///
/// Serialize and re-validate, then use the shared durable atomic writer to
/// replace the target, preserve its permissions, and sync its parent entry.
///
/// # Error conditions
/// - Serialization failure (should not happen with well-formed Values).
/// - Re-parse validation failure (internal bug).
/// - Cannot create parent directory.
/// - Atomic staging or publication failure (permissions, disk full).
///
/// In every error case the original file remains intact.
pub fn safe_write_json_file(
    path: &Path,
    value: &serde_json::Value,
    backup: Option<&Path>,
) -> Result<()> {
    // 1. Serialize
    let pretty = serde_json::to_string_pretty(value).map_err(|e| TraceDecayError::Config {
        message: format!("failed to serialize JSON for {}: {e}", path.display()),
    })?;

    // 2. Re-parse to verify the serialized output is valid JSON
    if serde_json::from_str::<serde_json::Value>(&pretty).is_err() {
        return Err(TraceDecayError::Config {
            message: format!(
                "internal error: serialized JSON for {} failed re-parse validation.\n  \
                 This is a bug in tracedecay — please report it.",
                path.display()
            ),
        });
    }

    let content = format!("{pretty}\n");
    safe_write_bytes_file(path, content.as_bytes(), backup)
}

/// Write text to a file via atomic sibling rename.
///
/// Mirrors [`safe_write_json_file`] for generated prompt/rule files that are
/// plain text rather than structured JSON. The target is not opened for writing
/// until the final rename, so a failed write leaves the original untouched.
pub fn safe_write_text_file(path: &Path, contents: &str, backup: Option<&Path>) -> Result<()> {
    safe_write_bytes_file(path, contents.as_bytes(), backup)
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct HostFileMetadataIdentityV1 {
    readonly: bool,
    unix_mode: Option<u32>,
    posix_acl_supported: bool,
    posix_acl_access: Option<Vec<u8>>,
}

pub fn capture_host_file_metadata(path: &Path) -> std::io::Result<HostFileMetadataIdentityV1> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !(metadata.is_file() || metadata.is_dir()) {
        return Err(std::io::Error::other(format!(
            "unsafe host metadata path: {}",
            path.display()
        )));
    }
    let permissions = metadata.permissions();
    #[cfg(unix)]
    let unix_mode = {
        use std::os::unix::fs::PermissionsExt;
        Some(permissions.mode())
    };
    #[cfg(not(unix))]
    let unix_mode = None;
    #[cfg(target_os = "linux")]
    let (posix_acl_supported, posix_acl_access) = match xattr::get(path, "system.posix_acl_access")
    {
        Ok(acl) => (true, acl),
        Err(error) if error.kind() == std::io::ErrorKind::Unsupported => (false, None),
        Err(error) => return Err(error),
    };
    #[cfg(not(target_os = "linux"))]
    let (posix_acl_supported, posix_acl_access) = (false, None);
    Ok(HostFileMetadataIdentityV1 {
        readonly: permissions.readonly(),
        unix_mode,
        posix_acl_supported,
        posix_acl_access,
    })
}

pub fn restore_host_file_metadata(
    path: &Path,
    state: &HostFileMetadataIdentityV1,
) -> std::io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !(metadata.is_file() || metadata.is_dir()) {
        return Err(std::io::Error::other(format!(
            "unsafe host metadata path: {}",
            path.display()
        )));
    }
    let mut permissions = metadata.permissions();
    #[cfg(unix)]
    if let Some(mode) = state.unix_mode {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(mode);
    }
    #[cfg(not(unix))]
    permissions.set_readonly(state.readonly);
    let current = std::fs::symlink_metadata(path)?;
    if current.file_type().is_symlink()
        || current.file_type() != metadata.file_type()
        || !(current.is_file() || current.is_dir())
    {
        return Err(std::io::Error::other(format!(
            "host metadata path changed type: {}",
            path.display()
        )));
    }
    std::fs::set_permissions(path, permissions)?;
    #[cfg(target_os = "linux")]
    if state.posix_acl_supported {
        let current = std::fs::symlink_metadata(path)?;
        if current.file_type().is_symlink() || !(current.is_file() || current.is_dir()) {
            return Err(std::io::Error::other(format!(
                "host metadata path changed type: {}",
                path.display()
            )));
        }
        match &state.posix_acl_access {
            Some(acl) => xattr::set(path, "system.posix_acl_access", acl)?,
            None => {
                if xattr::get(path, "system.posix_acl_access")?.is_some() {
                    xattr::remove(path, "system.posix_acl_access")?;
                }
            }
        }
    }
    Ok(())
}

/// Atomically replace a host-owned file while preserving existing permissions.
///
/// The shared durable writer syncs bytes and the parent entry. When replacing
/// an existing host config, restore its exact permission bits and durably flush
/// that metadata before returning. This authority is shared by every host:
/// existing config symlinks are always refused so no integration can redirect
/// a lifecycle write outside its inventoried path.
pub fn safe_write_bytes_file(path: &Path, contents: &[u8], backup: Option<&Path>) -> Result<()> {
    safe_write_bytes_file_with_metadata(path, contents, backup, None)
}

pub fn safe_write_bytes_file_with_metadata(
    path: &Path,
    contents: &[u8],
    backup: Option<&Path>,
    replacement_metadata: Option<&HostFileMetadataIdentityV1>,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| TraceDecayError::Config {
            message: format!("cannot create directory {}: {e}", parent.display()),
        })?;
    }
    let metadata = match std::fs::metadata(path) {
        Ok(_) => Some(
            capture_host_file_metadata(path).map_err(|e| TraceDecayError::Config {
                message: format!("failed to capture metadata for {}: {e}", path.display()),
            })?,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(TraceDecayError::Config {
                message: format!("failed to inspect metadata for {}: {error}", path.display()),
            });
        }
    };
    let publish_metadata = replacement_metadata.or(metadata.as_ref());
    if let Err(e) = tracedecay_application::atomic_write_prepared(
        path,
        "host-config",
        contents,
        |temporary| {
            if let Some(metadata) = publish_metadata {
                restore_host_file_metadata(temporary, metadata)?;
            }
            let expected_metadata = capture_host_file_metadata(temporary)?;
            persist_host_config_write_intent(path, contents, Some(&expected_metadata))
                .map_err(std::io::Error::other)?;
            Ok(())
        },
        tracedecay_application::DirectorySyncPolicy::TolerateUnsupported,
    ) {
        let hint = if let Some(b) = backup {
            format!(
                "\n  Backup is at: {}\n  \
                 The original file was NOT modified.",
                b.display()
            )
        } else {
            "\n  The original file was NOT modified.".to_string()
        };
        return Err(TraceDecayError::Config {
            message: format!("failed to atomically replace {}: {e}{hint}", path.display()),
        });
    }
    #[cfg(feature = "test-transport")]
    if (std::env::var_os("TRACEDECAY_TEST_ABORT_AFTER_HOST_CONFIG_WRITE").is_some()
        && !path
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .is_some_and(|name| name.ends_with(".bak") || name.ends_with(".tracedecay-original")))
        || std::env::var_os("TRACEDECAY_TEST_ABORT_AFTER_HOST_CONFIG_WRITE_PATH")
            .is_some_and(|expected| Path::new(&expected) == path)
    {
        std::process::abort();
    }
    Ok(())
}

thread_local! {
    static HOST_CONFIG_WRITE_INTENT_ROOT: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

pub fn with_host_config_write_intents<T>(root: PathBuf, effect: impl FnOnce() -> T) -> T {
    struct ResetIntentRoot(Option<PathBuf>);

    impl Drop for ResetIntentRoot {
        fn drop(&mut self) {
            HOST_CONFIG_WRITE_INTENT_ROOT.with(|current| {
                current.replace(self.0.take());
            });
        }
    }

    let previous = HOST_CONFIG_WRITE_INTENT_ROOT.with(|current| current.replace(Some(root)));
    let _reset = ResetIntentRoot(previous);
    effect()
}

pub fn host_config_write_intent_path(root: &Path, path: &Path) -> Result<PathBuf> {
    let path_bytes = serde_json::to_vec(path).map_err(|error| TraceDecayError::Config {
        message: format!("could not bind host config write intent: {error}"),
    })?;
    Ok(root.join(format!(
        "{}.intent",
        hex::encode(Sha256::digest(path_bytes))
    )))
}

pub fn safe_remove_host_file(path: &Path) -> std::io::Result<()> {
    persist_host_config_remove_intent(path).map_err(std::io::Error::other)?;
    std::fs::remove_file(path)?;
    #[cfg(feature = "test-transport")]
    if std::env::var_os("TRACEDECAY_TEST_ABORT_AFTER_HOST_CONFIG_REMOVE_PATH")
        .is_some_and(|expected| Path::new(&expected) == path)
    {
        std::process::abort();
    }
    Ok(())
}

#[derive(Serialize)]
struct HostConfigWriteIntentV2<'a> {
    schema_version: u16,
    digest: [u8; 32],
    metadata: Option<&'a HostFileMetadataIdentityV1>,
}

fn persist_host_config_write_intent(
    path: &Path,
    contents: &[u8],
    metadata: Option<&HostFileMetadataIdentityV1>,
) -> Result<()> {
    let Some(root) = HOST_CONFIG_WRITE_INTENT_ROOT.with(|current| current.borrow().clone()) else {
        return Ok(());
    };
    std::fs::create_dir_all(&root).map_err(|error| TraceDecayError::Config {
        message: format!(
            "could not create host config write intent directory {}: {error}",
            root.display()
        ),
    })?;
    let intent_path = host_config_write_intent_path(&root, path)?;
    let intent = serde_json::to_vec(&HostConfigWriteIntentV2 {
        schema_version: 2,
        digest: Sha256::digest(contents).into(),
        metadata,
    })
    .map_err(|error| TraceDecayError::Config {
        message: format!("could not serialize host config write intent: {error}"),
    })?;
    tracedecay_application::atomic_write(
        &intent_path,
        "host-config-intent",
        &intent,
        tracedecay_application::DirectorySyncPolicy::TolerateUnsupported,
    )
    .map_err(|error| TraceDecayError::Config {
        message: format!(
            "could not persist host config write intent {}: {error}",
            intent_path.display()
        ),
    })
}

fn persist_host_config_remove_intent(path: &Path) -> Result<()> {
    let Some(root) = HOST_CONFIG_WRITE_INTENT_ROOT.with(|current| current.borrow().clone()) else {
        return Ok(());
    };
    std::fs::create_dir_all(&root).map_err(|error| TraceDecayError::Config {
        message: format!(
            "could not create host config remove intent directory {}: {error}",
            root.display()
        ),
    })?;
    let intent_path = host_config_write_intent_path(&root, path)?;
    tracedecay_application::atomic_write(
        &intent_path,
        "host-config-remove-intent",
        &[0],
        tracedecay_application::DirectorySyncPolicy::TolerateUnsupported,
    )
    .map_err(|error| TraceDecayError::Config {
        message: format!(
            "could not persist host config remove intent {}: {error}",
            intent_path.display()
        ),
    })
}

/// Write a JSON value to a file with pretty formatting.
/// Creates a backup, writes atomically, and restores on failure.
pub fn write_json_file(path: &Path, value: &serde_json::Value) -> Result<()> {
    let backup = backup_config_file(path)?;
    safe_write_json_file(path, value, backup.as_deref())?;
    eprintln!("\x1b[32m✔\x1b[0m Wrote {}", path.display());
    Ok(())
}

/// Best-effort "back up and write" for uninstall paths.
///
/// Mirrors the install pattern (`backup_config_file` then
/// `safe_write_json_file`) but swallows errors so the rest of the uninstall
/// can continue. Returns `true` when the new content reached disk.
///
/// Issue #63: every config rewrite must leave a `.bak` so the user can
/// recover if anything goes wrong.
pub fn backup_and_write_json(path: &Path, value: &serde_json::Value) -> bool {
    let backup = backup_config_file(path).ok().flatten();
    safe_write_json_file(path, value, backup.as_deref()).is_ok()
}

// ---------------------------------------------------------------------------
// Shared MCP server registration
// ---------------------------------------------------------------------------
//
// Every JSON/JSONC-configured host registers tracedecay the same way: one
// entry named `tracedecay` under a root key (`mcpServers` for the Cline
// family and Gemini, `mcp` for Kilo). Only the config path, the root key, the
// entry shape, and the config dialect differ, so install, uninstall, and the
// doctor check all live here rather than once per host.

/// Register the tracedecay MCP entry under `root_key` in a host config.
///
/// `load_for_edit` is the host's strict loader ([`load_json_file_strict`] or
/// [`load_jsonc_file_strict`]): a config that exists but cannot be parsed is
/// an error rather than a silent overwrite. `agent_label` names the host in
/// the directory-creation error.
pub fn install_mcp_server_entry(
    config_path: &Path,
    root_key: &str,
    entry: serde_json::Value,
    agent_label: &str,
    load_for_edit: fn(&Path) -> Result<serde_json::Value>,
) -> Result<()> {
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| TraceDecayError::Config {
            message: format!(
                "cannot create {agent_label} config directory {}: {error}",
                parent.display()
            ),
        })?;
    }

    let backup = backup_config_file(config_path)?;
    let mut settings = match load_for_edit(config_path) {
        Ok(settings) => settings,
        Err(error) => {
            if let Some(ref backup) = backup {
                eprintln!("  Backup preserved at: {}", backup.display());
            }
            return Err(error);
        }
    };
    settings[root_key]["tracedecay"] = entry;

    safe_write_json_file(config_path, &settings, backup.as_deref())?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Added tracedecay MCP server to {}",
        config_path.display()
    );
    Ok(())
}

/// How far an uninstall prunes a config once the tracedecay entry is gone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct McpUninstallPolicy {
    /// Drop the MCP root key itself once it holds no servers.
    pub prune_empty_root: bool,
    /// Delete the config file when an empty MCP root is all that is left.
    pub remove_empty_file: bool,
}

/// Remove the tracedecay MCP entry under `root_key` from a host config.
///
/// Best effort by design: uninstall must keep going across the remaining
/// hosts when one config is unreadable or unwritable, so `load` is the host's
/// lenient loader ([`load_json_file`] or [`load_jsonc_file`]). Every rewrite
/// goes through [`backup_and_write_json`], so a `.bak` is always left behind
/// (issue #63).
pub fn uninstall_mcp_server_entry(
    config_path: &Path,
    root_key: &str,
    load: fn(&Path) -> serde_json::Value,
    policy: McpUninstallPolicy,
) {
    if !config_path.exists() {
        eprintln!("  {} not found, skipping", config_path.display());
        return;
    }

    let mut settings = load(config_path);
    let Some(servers) = settings.get_mut(root_key).and_then(|v| v.as_object_mut()) else {
        eprintln!(
            "  No tracedecay MCP server in {}, skipping",
            config_path.display()
        );
        return;
    };
    if servers.remove("tracedecay").is_none() {
        eprintln!(
            "  No tracedecay MCP server in {}, skipping",
            config_path.display()
        );
        return;
    }
    let root_is_empty = servers.is_empty();

    if policy.prune_empty_root
        && root_is_empty
        && let Some(object) = settings.as_object_mut()
    {
        object.remove(root_key);
    }

    if policy.remove_empty_file && config_holds_only_empty_root(&settings, root_key) {
        std::fs::remove_file(config_path).ok();
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed {} (was empty)",
            config_path.display()
        );
        return;
    }

    if backup_and_write_json(config_path, &settings) {
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed tracedecay MCP server from {}",
            config_path.display()
        );
    }
}

/// True when nothing survives in `settings` except an empty MCP root.
fn config_holds_only_empty_root(settings: &serde_json::Value, root_key: &str) -> bool {
    settings.as_object().is_some_and(|object| {
        object.iter().all(|(key, value)| {
            key == root_key && value.as_object().is_some_and(serde_json::Map::is_empty)
        })
    })
}

/// Host-specific wording for [`doctor_check_mcp_registration`].
pub struct McpDoctorLabels<'a> {
    /// Agent id used in the ``run `tracedecay install --agent <id>` `` hint.
    pub agent_id: &'a str,
    /// Product name used in the "if you use ..." warning for a missing file.
    pub product: &'a str,
    /// Subject of the pass line, rendered as "{registered} in {path}".
    pub registered: &'a str,
    /// Subject of the fail line, rendered as "{missing} in {path} — run ...".
    pub missing: &'a str,
}

/// Report whether a host config registers tracedecay under `root_key`.
///
/// Returns the registered server object so callers can keep checking
/// host-specific fields (Gemini's `args`/`trust`, for example).
pub fn doctor_check_mcp_registration(
    dc: &mut DoctorCounters,
    config_path: &Path,
    root_key: &str,
    load: fn(&Path) -> serde_json::Value,
    labels: &McpDoctorLabels<'_>,
) -> Option<serde_json::Value> {
    if !config_path.exists() {
        dc.warn(&format!(
            "{} not found — run `tracedecay install --agent {}` if you use {}",
            config_path.display(),
            labels.agent_id,
            labels.product
        ));
        return None;
    }

    let settings = load(config_path);
    let server = settings
        .get(root_key)
        .and_then(|servers| servers.get("tracedecay"))
        .filter(|server| server.is_object());

    if let Some(server) = server {
        dc.pass(&format!(
            "{} in {}",
            labels.registered,
            config_path.display()
        ));
        Some(server.clone())
    } else {
        dc.fail(&format!(
            "{} in {} — run `tracedecay install --agent {}`",
            labels.missing,
            config_path.display(),
            labels.agent_id
        ));
        None
    }
}

/// Finds the tracedecay binary path.
///
/// On Windows the returned path uses forward slashes so it can be safely
/// embedded in JSON hook commands without backslash-escaping issues.
pub fn which_tracedecay() -> Option<String> {
    which_tracedecay_path()
        .and_then(|path| path.to_str().map(|path| normalize_path_separators(path)))
}

/// Finds the tracedecay binary without converting its platform-native path.
pub fn which_tracedecay_path() -> Option<PathBuf> {
    let current_exe = std::env::current_exe().ok();
    let path_var = std::env::var_os("PATH");
    let cargo_target_dir = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from);
    which_tracedecay_path_from(
        current_exe.as_deref(),
        path_var.as_deref(),
        cargo_target_dir.as_deref(),
    )
}

#[cfg(test)]
fn which_tracedecay_from(
    current_exe: Option<&Path>,
    path_var: Option<&std::ffi::OsStr>,
    cargo_target_dir: Option<&Path>,
) -> Option<String> {
    which_tracedecay_path_from(current_exe, path_var, cargo_target_dir)
        .and_then(|path| path.to_str().map(|path| normalize_path_separators(path)))
}

fn which_tracedecay_path_from(
    current_exe: Option<&Path>,
    path_var: Option<&std::ffi::OsStr>,
    cargo_target_dir: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(exe) = current_exe
        .filter(|exe| is_tracedecay_exe(exe) && !is_cargo_target_binary(exe, cargo_target_dir))
    {
        return absolute_executable_path(exe);
    }

    let path_match = path_var.and_then(|path_var| {
        std::env::split_paths(path_var).find_map(|dir| {
            let candidate = dir.join(tracedecay_bin_name());
            (candidate.exists() && !is_cargo_target_binary(&candidate, cargo_target_dir))
                .then(|| absolute_executable_path(&candidate))
                .flatten()
        })
    });
    path_match.or_else(|| {
        current_exe
            .filter(|exe| is_tracedecay_exe(exe))
            .and_then(absolute_executable_path)
    })
}

fn absolute_executable_path(path: &Path) -> Option<PathBuf> {
    if path.is_absolute() {
        Some(path.to_path_buf())
    } else {
        std::path::absolute(path).ok()
    }
}

fn tracedecay_bin_name() -> String {
    format!("tracedecay{}", std::env::consts::EXE_SUFFIX)
}

fn is_tracedecay_exe(path: &Path) -> bool {
    path.file_stem()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            if cfg!(windows) {
                name.eq_ignore_ascii_case("tracedecay")
            } else {
                name == "tracedecay"
            }
        })
}

fn is_cargo_target_binary(path: &Path, cargo_target_dir: Option<&Path>) -> bool {
    if cargo_target_dir.is_some_and(|target_dir| path_starts_with_platform(path, target_dir)) {
        return true;
    }

    let mut saw_target = false;
    for component in path.components() {
        let value = component.as_os_str();
        if saw_target && (path_component_eq(value, "debug") || path_component_eq(value, "release"))
        {
            return true;
        }
        if path_component_eq(value, "target") {
            saw_target = true;
        }
    }
    false
}

fn path_starts_with_platform(path: &Path, prefix: &Path) -> bool {
    if !cfg!(windows) {
        return path.starts_with(prefix);
    }
    let mut path_components = path.components();
    prefix.components().all(|expected| {
        path_components
            .next()
            .is_some_and(|actual| path_component_eq(actual.as_os_str(), expected.as_os_str()))
    })
}

fn path_component_eq(actual: &std::ffi::OsStr, expected: impl AsRef<std::ffi::OsStr>) -> bool {
    let expected = expected.as_ref();
    if !cfg!(windows) {
        return actual == expected;
    }
    match (actual.to_str(), expected.to_str()) {
        (Some(actual), Some(expected)) => actual.eq_ignore_ascii_case(expected),
        _ => actual == expected,
    }
}

/// Replace backslashes with forward slashes so paths work in JSON/shell
/// contexts on Windows. No-op on Unix where paths already use `/`.
fn normalize_path_separators(path: &str) -> String {
    path.replace('\\', "/")
}

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

/// True when a `SKILL.md`'s contents carry a tracedecay authorship marker,
/// marking the skill dir as tracedecay-owned (and therefore safe to sweep when
/// retired). Shared by the Cursor and Codex plugin-dir sweeps.
pub(crate) fn skill_contents_have_tracedecay_marker(contents: &str) -> bool {
    contents.lines().map(str::trim).any(|line| {
        line.starts_with("name: tracedecay:")
            || line.starts_with("description: TraceDecay ")
            || line.contains("TraceDecay MCP")
            || line.contains("tracedecay_")
            || line.contains("`tracedecay:")
    })
}

/// Remove explicitly retired sibling plugin trees.
///
/// Both the retired suffix and ownership manifest are allow-listed: a name
/// prefix alone is never ownership evidence. A sibling is removed only when it
/// is a real directory, its suffix is known to have been created by
/// `TraceDecay`, and one host-specific manifest parses with `name = "tracedecay"`.
pub(crate) fn sweep_superseded_plugin_siblings(
    current_dir: &Path,
    ownership_manifests: &[&str],
) -> Result<()> {
    const RETIRED_SUFFIXES: &[&str] = &["pre-v2-adopt"];

    let Some(parent) = current_dir.parent() else {
        return Ok(());
    };
    let Some(current_name) = current_dir.file_name().and_then(|name| name.to_str()) else {
        return Ok(());
    };
    let prefix = format!("{current_name}.");
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(TraceDecayError::Config {
                message: format!(
                    "failed to inspect plugin siblings in {}: {error}",
                    parent.display()
                ),
            });
        }
    };

    for entry in entries {
        let entry = entry.map_err(|error| TraceDecayError::Config {
            message: format!(
                "failed to inspect a plugin sibling in {}: {error}",
                parent.display()
            ),
        })?;
        let file_type = entry.file_type().map_err(|error| TraceDecayError::Config {
            message: format!("failed to inspect {}: {error}", entry.path().display()),
        })?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let retired = name
            .strip_prefix(&prefix)
            .is_some_and(|suffix| RETIRED_SUFFIXES.contains(&suffix));
        if !file_type.is_dir() || !retired {
            continue;
        }
        let sibling = entry.path();
        let owned = ownership_manifests.iter().any(|relative| {
            load_json_file(&sibling.join(relative))
                .get("name")
                .and_then(serde_json::Value::as_str)
                == Some("tracedecay")
        });
        if !owned {
            continue;
        }
        std::fs::remove_dir_all(&sibling).map_err(|error| TraceDecayError::Config {
            message: format!(
                "failed to remove superseded tracedecay plugin {}: {error}",
                sibling.display()
            ),
        })?;
    }
    Ok(())
}

/// Recursively collect every regular file under `root` (following the same
/// hand-rolled walk both the Cursor and Codex installers rely on).
pub(crate) fn collect_regular_files(root: &Path) -> std::io::Result<Vec<PathBuf>> {
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

pub(crate) fn hook_command(tracedecay_bin: &str, subcommand: &str) -> String {
    hook_command_for_platform(tracedecay_bin, subcommand, cfg!(windows))
}

pub(crate) fn hook_command_for_platform(
    tracedecay_bin: &str,
    subcommand: &str,
    windows: bool,
) -> String {
    let quoted = if windows {
        quote_windows_command_arg(&normalize_path_separators(tracedecay_bin))
    } else {
        quote_posix_command_arg(tracedecay_bin)
    };
    format!("{quoted} {subcommand}")
}

fn quote_windows_command_arg(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\\\""))
}

fn quote_posix_command_arg(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn canonicalize_existing_prefix(path: &Path) -> std::io::Result<PathBuf> {
    let mut existing = path.to_path_buf();
    let mut missing = Vec::new();

    loop {
        match existing.canonicalize() {
            Ok(mut canonical) => {
                for component in missing.iter().rev() {
                    canonical.push(component);
                }
                return Ok(canonical);
            }
            Err(err) => {
                let Some(name) = existing.file_name().map(std::borrow::ToOwned::to_owned) else {
                    return Err(err);
                };
                missing.push(name);
                if !existing.pop() {
                    return Err(err);
                }
            }
        }
    }
}

fn relative_project_path(
    project_root: &Path,
    canonical_root: &Path,
    absolute: &Path,
    original: &Path,
) -> Option<PathBuf> {
    if !original.is_absolute() {
        return Some(original.to_path_buf());
    }
    absolute
        .strip_prefix(project_root)
        .or_else(|_| absolute.strip_prefix(canonical_root))
        .ok()
        .map(Path::to_path_buf)
}

pub(crate) fn ensure_project_local_safe_path(project_root: &Path, path: &Path) -> Result<()> {
    let root = project_root
        .canonicalize()
        .map_err(|e| TraceDecayError::Config {
            message: format!(
                "failed to resolve project root {}: {e}",
                project_root.display()
            ),
        })?;
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    if absolute
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(TraceDecayError::Config {
            message: format!(
                "refusing to write project-local config outside {}: {}",
                root.display(),
                absolute.display()
            ),
        });
    }

    if let Some(relative) = relative_project_path(project_root, &root, &absolute, path) {
        let scan_root = if project_root.is_absolute() {
            project_root.to_path_buf()
        } else {
            root.clone()
        };
        let mut current = scan_root;
        for component in relative.components() {
            if matches!(
                component,
                std::path::Component::Prefix(_) | std::path::Component::RootDir
            ) {
                continue;
            }
            current.push(component.as_os_str());
            let Ok(meta) = std::fs::symlink_metadata(&current) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "refusing to write project-local config through symlink: {}",
                        current.display()
                    ),
                });
            }
        }
    }

    let canonical_candidate =
        canonicalize_existing_prefix(&absolute).map_err(|e| TraceDecayError::Config {
            message: format!(
                "failed to resolve project-local config path {}: {e}",
                absolute.display()
            ),
        })?;
    if !canonical_candidate.starts_with(&root) {
        return Err(TraceDecayError::Config {
            message: format!(
                "refusing to write project-local config outside {}: {}",
                root.display(),
                absolute.display()
            ),
        });
    }

    Ok(())
}

/// Guard every project-local write target up front: reject any path that
/// escapes `project_root` or reaches through a symlinked parent before the
/// installer creates directories or writes files. Mirrors the per-path
/// [`ensure_project_local_safe_path`] contract for adapters that touch several
/// project-local paths in one `install_local`.
pub(crate) fn ensure_project_local_safe_paths<'a, I>(project_root: &Path, paths: I) -> Result<()>
where
    I: IntoIterator<Item = &'a Path>,
{
    for path in paths {
        ensure_project_local_safe_path(project_root, path)?;
    }
    Ok(())
}

/// Returns the user's home directory, cross-platform.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .map(PathBuf::from)
}

/// Strip `//` line comments, `/* */` block comments, and trailing commas
/// before `}` / `]` from a JSONC string, then parse with `serde_json`.
/// Falls back to `serde_json::json!({})` on any parse failure.
pub fn parse_jsonc(input: &str) -> serde_json::Value {
    let stripped = strip_jsonc_comments(input);
    serde_json::from_str(&stripped).unwrap_or_else(|_| serde_json::json!({}))
}

/// Internal helper: removes JSONC comments and trailing commas.
fn strip_jsonc_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let len = chars.len();
    let mut i = 0;
    let mut in_string = false;

    while i < len {
        // Handle string literals (skip comment stripping inside strings).
        if in_string {
            if chars[i] == '\\' && i + 1 < len {
                out.push(chars[i]);
                out.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if chars[i] == '"' {
                in_string = false;
            }
            out.push(chars[i]);
            i += 1;
            continue;
        }

        // Start of string.
        if chars[i] == '"' {
            in_string = true;
            out.push(chars[i]);
            i += 1;
            continue;
        }

        // Line comment `//`.
        if chars[i] == '/' && i + 1 < len && chars[i + 1] == '/' {
            // Skip until newline.
            while i < len && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }

        // Block comment `/* ... */`.
        if chars[i] == '/' && i + 1 < len && chars[i + 1] == '*' {
            i += 2;
            while i + 1 < len && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i += 2; // consume `*/`
            continue;
        }

        out.push(chars[i]);
        i += 1;
    }

    // Remove trailing commas before `}` or `]`.
    // Simple regex-free approach: repeatedly collapse ", <whitespace> }" patterns.
    remove_trailing_commas(&out)
}

/// Removes trailing commas that appear immediately before `}` or `]` (with
/// optional whitespace/newlines in between).
fn remove_trailing_commas(input: &str) -> String {
    // We scan for comma, optional whitespace, then `}` or `]`.
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut out = Vec::with_capacity(len);
    let mut i = 0;

    while i < len {
        if bytes[i] == b',' {
            // Peek ahead past whitespace.
            let mut j = i + 1;
            while j < len
                && (bytes[j] == b' ' || bytes[j] == b'\t' || bytes[j] == b'\n' || bytes[j] == b'\r')
            {
                j += 1;
            }
            if j < len && (bytes[j] == b'}' || bytes[j] == b']') {
                // Skip the comma; whitespace will be included normally.
                i += 1;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }

    String::from_utf8(out).unwrap_or_else(|_| input.to_string())
}

/// Read a file and parse it as JSONC. Falls back to `json!({})` if the file
/// is missing, unreadable, or unparseable.
/// Use this for **read-only** paths. For install/edit paths, use
/// [`load_jsonc_file_strict`] instead.
pub fn load_jsonc_file(path: &Path) -> serde_json::Value {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return serde_json::json!({});
    };
    parse_jsonc(&contents)
}

/// Load a JSONC file for **editing**. Unlike [`load_jsonc_file`], this returns
/// an error if the file exists but cannot be parsed after comment stripping,
/// preventing silent data loss when the modified value is written back.
///
/// # Error conditions
/// - File exists but is not readable (permissions, I/O error).
/// - File exists and has content but contains invalid JSONC.
///
/// Returns `Ok(json!({}))` only when the file does not exist or is empty.
pub fn load_jsonc_file_strict(path: &Path) -> Result<serde_json::Value> {
    if !path.exists() {
        return Ok(serde_json::json!({}));
    }
    let contents = std::fs::read_to_string(path).map_err(|e| TraceDecayError::Config {
        message: format!("cannot read {}: {e}", path.display()),
    })?;
    if contents.trim().is_empty() {
        return Ok(serde_json::json!({}));
    }
    let stripped = strip_jsonc_comments(&contents);
    serde_json::from_str(&stripped).map_err(|e| TraceDecayError::Config {
        message: format!(
            "cannot parse {} as JSONC: {e}\n  \
             Hint: fix the JSON syntax manually and re-run the command,\n  \
             or delete the file to start fresh",
            path.display()
        ),
    })
}

/// Returns the VS Code user data directory, platform-specific.
pub fn vscode_data_dir(home: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home.join("Library/Application Support/Code")
    }
    #[cfg(target_os = "linux")]
    {
        home.join(".config/Code")
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            let appdata_path = PathBuf::from(&appdata);
            if appdata_path.starts_with(home) {
                return appdata_path.join("Code");
            }
        }
        home.join("AppData/Roaming/Code")
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        home.join(".config/Code")
    }
}

/// Returns the platform-specific VS Code Insiders data directory.
pub fn vscode_insiders_data_dir(home: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home.join("Library/Application Support/Code - Insiders")
    }
    #[cfg(target_os = "linux")]
    {
        home.join(".config/Code - Insiders")
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            let appdata_path = PathBuf::from(&appdata);
            if appdata_path.starts_with(home) {
                return appdata_path.join("Code - Insiders");
            }
        }
        home.join("AppData/Roaming/Code - Insiders")
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        home.join(".config/Code - Insiders")
    }
}

/// Returns the GitHub Copilot CLI config directory.
pub fn copilot_cli_dir(home: &Path) -> PathBuf {
    home.join(".copilot")
}

/// Returns the Kiro IDE user data directory (VS Code-style layout).
pub fn kiro_data_dir(home: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home.join("Library/Application Support/Kiro")
    }
    #[cfg(target_os = "linux")]
    {
        home.join(".config/Kiro")
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            let appdata_path = PathBuf::from(&appdata);
            if appdata_path.starts_with(home) {
                return appdata_path.join("Kiro");
            }
        }
        home.join("AppData/Roaming/Kiro")
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        home.join(".config/Kiro")
    }
}

/// Returns agent IDs that have tracedecay configured under `home` but are
/// absent from `current`. Pure — does no I/O on the config file.
pub fn detect_missing_installed_agents(home: &Path, current: &[String]) -> Vec<String> {
    let mut additions = Vec::new();
    for ag in all_integrations() {
        let id = ag.id().to_string();
        if ag.has_tracedecay(home) && !current.contains(&id) {
            additions.push(id);
        }
    }
    additions
}

/// The tracked-agent list this crate backfills, as a port over the root
/// crate's `user_config::UserConfig`.
///
/// `UserConfig` stays above this crate — it is the whole user-level profile,
/// most of which host detection has no business seeing. The backfill below
/// needs exactly three operations, so it takes them as a port rather than the
/// concrete type.
///
/// Root wiring: the root implements this for `user_config::UserConfig`
/// (`installed_agents` field reads/extends, and its existing `save`).
pub trait InstalledAgentsConfig {
    /// Agent ids currently recorded as installed.
    fn installed_agents(&self) -> &[String];

    /// Appends newly detected agent ids to the tracked list.
    fn extend_installed_agents(&mut self, additions: Vec<String>);

    /// Persists the config. Failures are logged, never fatal: a lost backfill
    /// is retried on the next run.
    fn save(&self) -> Result<()>;
}

/// Backfill `installed_agents` for users upgrading from older versions.
///
/// Always scans every agent and adds any that have tracedecay configured
/// (e.g. an `~/.claude.json` MCP server entry) but are absent from
/// `installed_agents`. Without the additive scan, a user who installed
/// agent A first and agent B later would have only A in the list, so
/// `tracedecay reinstall` would silently skip B and its tool permissions
/// would never be refreshed when new tools ship.
pub fn migrate_installed_agents(home: &Path, config: &mut dyn InstalledAgentsConfig) {
    let additions = detect_missing_installed_agents(home, config.installed_agents());
    if additions.is_empty() {
        return;
    }
    config.extend_installed_agents(additions);
    if let Err(err) = config.save() {
        tracing::warn!(%err, "could not save tracedecay config");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod migrate_tests {
    use super::*;
    use std::fs;

    /// Writes a minimal `~/.claude.json` so `ClaudeIntegration::has_tracedecay`
    /// returns true for the given fake home.
    fn install_claude_marker(home: &Path) {
        let claude_json = home.join(".claude.json");
        fs::write(
            &claude_json,
            r#"{"mcpServers":{"tracedecay":{"command":"tracedecay","args":["serve"]}}}"#,
        )
        .unwrap();
    }

    /// Regression test for the bug where `tracedecay reinstall` skipped Claude
    /// when another agent (e.g. copilot) was already in `installed_agents`.
    /// `migrate_installed_agents` previously returned early as soon as the
    /// list was non-empty, so Claude never got tracked and its tool perms
    /// never refreshed.
    #[test]
    fn detects_claude_when_another_agent_already_tracked() {
        let dir = tempfile::tempdir().unwrap();
        install_claude_marker(dir.path());

        let current = vec!["copilot".to_string()];
        let additions = detect_missing_installed_agents(dir.path(), &current);

        assert!(
            additions.iter().any(|id| id == "claude"),
            "claude must be detected even when copilot is already in the list, got {additions:?}"
        );
    }

    #[test]
    fn detects_claude_when_list_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        install_claude_marker(dir.path());

        let additions = detect_missing_installed_agents(dir.path(), &[]);

        assert!(additions.iter().any(|id| id == "claude"));
    }

    #[test]
    fn no_additions_when_claude_already_tracked() {
        let dir = tempfile::tempdir().unwrap();
        install_claude_marker(dir.path());

        let current = vec!["claude".to_string()];
        let additions = detect_missing_installed_agents(dir.path(), &current);

        assert!(
            !additions.contains(&"claude".to_string()),
            "claude is already tracked; must not be re-added, got {additions:?}"
        );
    }

    #[test]
    fn empty_home_yields_no_additions() {
        let dir = tempfile::tempdir().unwrap();
        let additions = detect_missing_installed_agents(dir.path(), &[]);
        assert!(
            additions.is_empty(),
            "no agent files in home → no additions, got {additions:?}"
        );
    }
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

/// Load a TOML file as a document.
///
/// Returns an empty table when the file does not exist. When the file exists
/// but cannot be parsed as a TOML document, returns a [`TraceDecayError::Config`]
/// so callers do not silently overwrite the user's data (see issue #63).
pub fn load_toml_file(path: &Path) -> Result<toml::Value> {
    if !path.exists() {
        return Ok(toml::Value::Table(toml::map::Map::new()));
    }
    let contents = std::fs::read_to_string(path).map_err(|e| TraceDecayError::Config {
        message: format!("failed to read {}: {e}", path.display()),
    })?;
    if contents.trim().is_empty() {
        return Ok(toml::Value::Table(toml::map::Map::new()));
    }
    // NOTE: `str.parse::<toml::Value>()` parses a single TOML value in toml v1,
    // not a document — using it here would treat any well-formed config.toml as
    // unparseable and silently drop its contents. Use `toml::from_str` instead.
    let table: toml::Table = toml::from_str(&contents).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to parse {} as TOML: {e}. Refusing to overwrite — fix the file or remove it manually.",
            path.display()
        ),
    })?;
    Ok(toml::Value::Table(table))
}

/// Copy `path` to `<path>.bak` if it exists. Used before overwriting a user
/// config so an unexpected change is recoverable (issue #63).
fn backup_file(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let mut backup = path.as_os_str().to_owned();
    backup.push(".bak");
    let backup = std::path::PathBuf::from(backup);
    std::fs::copy(path, &backup).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to back up {} to {}: {e}",
            path.display(),
            backup.display()
        ),
    })?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Backed up {} to {}",
        path.display(),
        backup.display()
    );
    Ok(())
}

/// Write a TOML value to a file, backing up any existing file first.
pub fn write_toml_file(path: &Path, value: &toml::Value) -> Result<()> {
    backup_file(path)?;
    let contents = toml::to_string_pretty(value).unwrap_or_else(|_| String::new());
    std::fs::write(path, contents).map_err(|e| TraceDecayError::Config {
        message: format!("failed to write {}: {e}", path.display()),
    })?;
    eprintln!("\x1b[32m✔\x1b[0m Wrote {}", path.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// Git post-commit hook
// ---------------------------------------------------------------------------

/// The marker comment used to identify tracedecay's section in a hook script.
///
/// NOTE: Legacy hooks written by the old "tracedecay" binary used the marker
/// "# tracedecay: auto-sync". Those are not detected by this constant, so
/// existing tracedecay git hooks will not be treated as already-present and a
/// second tracedecay block may be appended on offer. This is intentional
/// (install path only writes new identity) — users can manually remove the
/// old block.
const HOOK_MARKER: &str = "# tracedecay: auto-sync";

/// The hook snippet appended to (or written as) the post-commit script.
fn post_commit_snippet(tracedecay_bin: &str) -> String {
    let bin = quote_posix_command_arg(&tracedecay_bin.replace('\\', "/"));
    format!(
        "{HOOK_MARKER}\n\
         {bin} sync >/dev/null 2>&1 &\n"
    )
}

/// If a global git `post-commit` hook is not already set up for tracedecay,
/// interactively asks the user whether to install one. Silently succeeds if
/// the hook is already present, if stdin is not a terminal, or if the user
/// declines.
pub fn offer_git_post_commit_hook(tracedecay_bin: &str) {
    let Some(home) = home_dir() else { return };

    // Determine the global hooks directory by reading core.hooksPath from
    // the global gitconfig file(s). Falls back to ~/.config/git/hooks/.
    let hooks_dir = read_global_hooks_path(&home);

    let (hooks_dir, need_set_hookspath) = match hooks_dir {
        Some(dir) => (dir, false),
        None => (home.join(".config").join("git").join("hooks"), true),
    };

    let hook_path = hooks_dir.join("post-commit");

    // Check if already installed.
    if hook_path.exists()
        && let Ok(contents) = std::fs::read_to_string(&hook_path)
        && contents.contains(HOOK_MARKER)
    {
        eprintln!("  Global git post-commit hook already contains tracedecay, skipping");
        return;
    }

    // Only prompt on a real terminal.
    if !atty_stdin() {
        return;
    }

    eprintln!();
    eprint!(
        "Install a global git post-commit hook to auto-run \x1b[1mtracedecay sync\x1b[0m after each commit? [y/N] "
    );

    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return;
    }
    if !matches!(answer.trim(), "y" | "Y" | "yes" | "Yes") {
        eprintln!("  Skipped git post-commit hook");
        return;
    }

    // Create the hooks directory if needed.
    if let Err(e) = std::fs::create_dir_all(&hooks_dir) {
        eprintln!(
            "  \x1b[31m✘\x1b[0m Failed to create {}: {e}",
            hooks_dir.display()
        );
        return;
    }

    // If no global hooksPath was configured, set it in ~/.gitconfig.
    if need_set_hookspath {
        let gitconfig_path = home.join(".gitconfig");
        if let Err(msg) = set_global_hooks_path(&gitconfig_path, &hooks_dir) {
            eprintln!("  \x1b[31m✘\x1b[0m {msg} — hook not installed");
            return;
        }
        eprintln!(
            "\x1b[32m✔\x1b[0m Set git core.hooksPath to {}",
            hooks_dir.display()
        );
    }

    // Append to or create the hook file.
    let snippet = post_commit_snippet(tracedecay_bin);

    if hook_path.exists() {
        use std::io::Write;
        let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&hook_path) else {
            eprintln!(
                "  \x1b[31m✘\x1b[0m Failed to open {} for writing",
                hook_path.display()
            );
            return;
        };
        if write!(f, "\n{snippet}").is_err() {
            eprintln!(
                "  \x1b[31m✘\x1b[0m Failed to write to {}",
                hook_path.display()
            );
            return;
        }
    } else {
        let contents = format!("#!/bin/sh\n{snippet}");
        if std::fs::write(&hook_path, contents).is_err() {
            eprintln!(
                "  \x1b[31m✘\x1b[0m Failed to create {}",
                hook_path.display()
            );
            return;
        }
    }

    // Make executable (Unix).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755));
    }

    eprintln!(
        "\x1b[32m✔\x1b[0m Installed global git post-commit hook at {}",
        hook_path.display()
    );
}

/// Reads `core.hooksPath` from the global gitconfig files.
///
/// Checks `~/.gitconfig` first, then `~/.config/git/config` (the XDG
/// location). Returns the resolved absolute path, or `None` if the key
/// is absent from both files.
fn read_global_hooks_path(home: &Path) -> Option<PathBuf> {
    let candidates = [
        home.join(".gitconfig"),
        home.join(".config").join("git").join("config"),
    ];
    for path in &candidates {
        if let Some(value) = parse_gitconfig_value(path, "core", "hookspath") {
            let expanded = expand_tilde(&value, home);
            let p = PathBuf::from(&expanded);
            if p.is_absolute() {
                return Some(p);
            }
            // Relative paths in gitconfig are relative to the home dir.
            return Some(home.join(p));
        }
    }
    None
}

/// Minimal gitconfig parser: finds the value of `key` under `[section]`.
///
/// Key matching is case-insensitive (git config keys are case-insensitive).
/// Handles `key = value`, `key=value`, and quoted values.
fn parse_gitconfig_value(path: &Path, section: &str, key: &str) -> Option<String> {
    let contents = std::fs::read_to_string(path).ok()?;
    let section_lower = section.to_ascii_lowercase();
    let key_lower = key.to_ascii_lowercase();

    let mut in_section = false;
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            // Parse section header: [core], [core "subsection"], etc.
            let header = trimmed
                .trim_start_matches('[')
                .split(']')
                .next()
                .unwrap_or("")
                .trim();
            let section_name = header.split_whitespace().next().unwrap_or("");
            in_section = section_name.eq_ignore_ascii_case(&section_lower);
            continue;
        }
        if !in_section {
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        // Parse key = value
        if let Some((k, v)) = trimmed.split_once('=')
            && k.trim().to_ascii_lowercase() == key_lower
        {
            let v = v.trim();
            // Strip surrounding quotes if present.
            let v = v
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .unwrap_or(v);
            return Some(v.to_string());
        }
    }
    None
}

/// Appends `core.hooksPath` to the global gitconfig file, creating it if
/// necessary. Appends to an existing `[core]` section if one exists,
/// otherwise adds a new one at the end of the file.
fn set_global_hooks_path(
    gitconfig_path: &Path,
    hooks_dir: &Path,
) -> std::result::Result<(), String> {
    let hooks_str = hooks_dir.to_string_lossy().replace('\\', "/");
    let contents = if gitconfig_path.exists() {
        std::fs::read_to_string(gitconfig_path)
            .map_err(|e| format!("Failed to read {}: {e}", gitconfig_path.display()))?
    } else {
        String::new()
    };

    let new_contents = insert_gitconfig_value(&contents, "core", "hooksPath", &hooks_str);

    if let Some(parent) = gitconfig_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create {}: {e}", parent.display()))?;
    }
    std::fs::write(gitconfig_path, new_contents)
        .map_err(|e| format!("Failed to write {}: {e}", gitconfig_path.display()))?;
    Ok(())
}

/// Inserts `key = value` under `[section]` in gitconfig content.
/// If the section exists, appends the key after the last line of that section.
/// Otherwise appends a new section at the end.
fn insert_gitconfig_value(contents: &str, section: &str, key: &str, value: &str) -> String {
    let section_lower = section.to_ascii_lowercase();
    let lines: Vec<&str> = contents.lines().collect();
    let mut result = Vec::with_capacity(lines.len() + 3);
    let entry = format!("\t{key} = {value}");

    // Find the target section and the line index just before the next section.
    let mut section_end: Option<usize> = None;
    let mut in_section = false;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            if in_section {
                // We've hit the next section — insert before it.
                section_end = Some(i);
                break;
            }
            let header = trimmed
                .trim_start_matches('[')
                .split(']')
                .next()
                .unwrap_or("")
                .trim();
            let name = header.split_whitespace().next().unwrap_or("");
            if name.eq_ignore_ascii_case(&section_lower) {
                in_section = true;
            }
        }
    }
    if in_section && section_end.is_none() {
        // Section runs to end of file.
        section_end = Some(lines.len());
    }

    if let Some(insert_at) = section_end {
        for (i, line) in lines.iter().enumerate() {
            if i == insert_at {
                result.push(entry.as_str());
            }
            result.push(line);
        }
        // If inserting at end-of-file.
        if insert_at == lines.len() {
            result.push(&entry);
        }
    } else {
        // Section doesn't exist — append it.
        for line in &lines {
            result.push(line);
        }
        if !contents.is_empty() && !contents.ends_with('\n') {
            result.push("");
        }
        let section_header = format!("[{section}]");
        // We need to own these strings for the result.
        // Re-build as a String directly instead.
        let mut out = result.join("\n");
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&section_header);
        out.push('\n');
        out.push_str(&entry);
        out.push('\n');
        return out;
    }

    let mut out = result.join("\n");
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Expand a leading `~` to the given home directory.
fn expand_tilde(s: &str, home: &Path) -> String {
    if let Some(rest) = s.strip_prefix("~/") {
        return home.join(rest).to_string_lossy().replace('\\', "/");
    }
    if s == "~" {
        return home.to_string_lossy().to_string();
    }
    s.to_string()
}

/// Returns true if stdin is connected to a terminal.
fn atty_stdin() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod git_hook_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn parse_hookspath_basic() {
        let config = "[core]\n\thooksPath = /home/user/.git-hooks\n";
        assert_eq!(
            parse_gitconfig_value_from_str(config, "core", "hookspath"),
            Some("/home/user/.git-hooks".to_string())
        );
    }

    #[test]
    fn parse_hookspath_quoted() {
        let config = "[core]\n\thooksPath = \"/home/user/my hooks\"\n";
        assert_eq!(
            parse_gitconfig_value_from_str(config, "core", "hookspath"),
            Some("/home/user/my hooks".to_string())
        );
    }

    #[test]
    fn parse_hookspath_case_insensitive() {
        let config = "[Core]\n\tHooksPath = /tmp/hooks\n";
        assert_eq!(
            parse_gitconfig_value_from_str(config, "core", "hookspath"),
            Some("/tmp/hooks".to_string())
        );
    }

    #[test]
    fn parse_hookspath_missing() {
        let config = "[core]\n\tautocrlf = true\n";
        assert_eq!(
            parse_gitconfig_value_from_str(config, "core", "hookspath"),
            None
        );
    }

    #[test]
    fn parse_hookspath_wrong_section() {
        let config = "[user]\n\thooksPath = /nope\n[core]\n\tautocrlf = true\n";
        assert_eq!(
            parse_gitconfig_value_from_str(config, "core", "hookspath"),
            None
        );
    }

    #[test]
    fn insert_into_existing_section() {
        let config = "[user]\n\tname = Test\n[core]\n\tautocrlf = true\n";
        let result = insert_gitconfig_value(config, "core", "hooksPath", "/tmp/hooks");
        assert!(result.contains("\thooksPath = /tmp/hooks"));
        assert!(result.contains("[core]"));
        assert!(result.contains("autocrlf = true"));
    }

    #[test]
    fn insert_new_section() {
        let config = "[user]\n\tname = Test\n";
        let result = insert_gitconfig_value(config, "core", "hooksPath", "/tmp/hooks");
        assert!(result.contains("[core]\n\thooksPath = /tmp/hooks"));
    }

    #[test]
    fn insert_into_empty_file() {
        let result = insert_gitconfig_value("", "core", "hooksPath", "/tmp/hooks");
        assert!(result.contains("[core]\n\thooksPath = /tmp/hooks"));
    }

    #[test]
    fn insert_before_next_section() {
        let config = "[core]\n\tautocrlf = true\n[user]\n\tname = Test\n";
        let result = insert_gitconfig_value(config, "core", "hooksPath", "/tmp/hooks");
        // hooksPath should appear after autocrlf but before [user]
        let hooks_pos = result.find("hooksPath").unwrap();
        let user_pos = result.find("[user]").unwrap();
        let autocrlf_pos = result.find("autocrlf").unwrap();
        assert!(hooks_pos > autocrlf_pos);
        assert!(hooks_pos < user_pos);
    }

    #[test]
    fn expand_tilde_with_slash() {
        let home = Path::new("/home/test");
        assert_eq!(expand_tilde("~/hooks", home), "/home/test/hooks");
    }

    #[test]
    fn expand_tilde_bare() {
        let home = Path::new("/home/test");
        assert_eq!(expand_tilde("~", home), "/home/test");
    }

    #[test]
    fn expand_tilde_no_tilde() {
        let home = Path::new("/home/test");
        assert_eq!(expand_tilde("/abs/path", home), "/abs/path");
    }

    /// Helper: parse from a string directly (avoids file I/O in tests).
    fn parse_gitconfig_value_from_str(contents: &str, section: &str, key: &str) -> Option<String> {
        let section_lower = section.to_ascii_lowercase();
        let key_lower = key.to_ascii_lowercase();
        let mut in_section = false;
        for line in contents.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') {
                let header = trimmed
                    .trim_start_matches('[')
                    .split(']')
                    .next()
                    .unwrap_or("")
                    .trim();
                let section_name = header.split_whitespace().next().unwrap_or("");
                in_section = section_name.eq_ignore_ascii_case(&section_lower);
                continue;
            }
            if !in_section {
                continue;
            }
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
                continue;
            }
            if let Some((k, v)) = trimmed.split_once('=')
                && k.trim().to_ascii_lowercase() == key_lower
            {
                let v = v.trim();
                let v = v
                    .strip_prefix('"')
                    .and_then(|s| s.strip_suffix('"'))
                    .unwrap_or(v);
                return Some(v.to_string());
            }
        }
        None
    }
}

pub fn tool_names() -> Vec<String> {
    advertised_tools()
        .into_iter()
        .map(|tool| tool.name)
        .collect()
}

pub fn read_only_tool_names() -> Vec<String> {
    advertised_tools()
        .into_iter()
        .filter(|tool| tool.read_only)
        .map(|tool| tool.name)
        .collect()
}

pub fn expected_tool_perms() -> Vec<String> {
    advertised_tools()
        .iter()
        .map(|tool| format!("mcp__tracedecay__{}", tool.name))
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod jsonc_tests {
    use super::*;

    #[test]
    fn parse_jsonc_plain_json() {
        let input = r#"{"key": "value", "num": 42}"#;
        let v = parse_jsonc(input);
        assert_eq!(v["key"], "value");
        assert_eq!(v["num"], 42);
    }

    #[test]
    fn parse_jsonc_line_comment() {
        let input = "{\n  // this is a comment\n  \"key\": \"val\"\n}";
        let v = parse_jsonc(input);
        assert_eq!(v["key"], "val");
    }

    #[test]
    fn parse_jsonc_block_comment() {
        let input = "{ /* block comment */ \"key\": \"val\" }";
        let v = parse_jsonc(input);
        assert_eq!(v["key"], "val");
    }

    #[test]
    fn parse_jsonc_trailing_comma_object() {
        let input = r#"{"a": 1, "b": 2,}"#;
        let v = parse_jsonc(input);
        assert_eq!(v["a"], 1);
        assert_eq!(v["b"], 2);
    }

    #[test]
    fn parse_jsonc_trailing_comma_array() {
        let input = r#"{"items": [1, 2, 3,]}"#;
        let v = parse_jsonc(input);
        assert_eq!(v["items"][2], 3);
    }

    #[test]
    fn parse_jsonc_combined() {
        let input = "{\n  // comment\n  \"x\": /* inline */ 99,\n}";
        let v = parse_jsonc(input);
        assert_eq!(v["x"], 99);
    }

    #[test]
    fn parse_jsonc_url_in_string_not_stripped() {
        // A URL containing `//` inside a string must NOT be treated as a comment.
        let input = r#"{"url": "https://example.com/path"}"#;
        let v = parse_jsonc(input);
        assert_eq!(v["url"], "https://example.com/path");
    }

    #[test]
    fn parse_jsonc_invalid_falls_back_to_empty() {
        let input = "not valid json at all !!!";
        let v = parse_jsonc(input);
        assert_eq!(v, serde_json::json!({}));
    }

    #[test]
    fn parse_jsonc_empty_string() {
        let v = parse_jsonc("");
        assert_eq!(v, serde_json::json!({}));
    }

    #[test]
    fn parse_jsonc_trailing_comma_with_whitespace() {
        let input = "{\n  \"a\": 1  ,\n}";
        let v = parse_jsonc(input);
        assert_eq!(v["a"], 1);
    }
}

// ---------------------------------------------------------------------------
// Regression tests for safe config backup / load / write
// ---------------------------------------------------------------------------
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod safe_config_tests {
    use super::*;
    use std::fs;

    /// Create a temp directory that is cleaned up on drop.
    fn tmpdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("failed to create temp dir")
    }

    // ----- backup_config_file -----

    #[test]
    fn backup_returns_none_when_file_missing() {
        let dir = tmpdir();
        let path = dir.path().join("nonexistent.json");
        let result = backup_config_file(&path).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn backup_creates_bak_with_identical_content() {
        let dir = tmpdir();
        let path = dir.path().join("config.json");
        let original = r#"{"existing": "data", "nested": {"key": 1}}"#;
        fs::write(&path, original).unwrap();

        let backup = backup_config_file(&path)
            .unwrap()
            .expect("should create backup");
        assert!(backup.exists());
        assert_eq!(fs::read_to_string(&backup).unwrap(), original);
        // Original is untouched
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn backup_staging_file_is_cleaned_up() {
        let dir = tmpdir();
        let path = dir.path().join("config.json");
        fs::write(&path, "{}").unwrap();

        backup_config_file(&path).unwrap();

        let staging = dir.path().join("config.json.bak.new");
        assert!(!staging.exists(), ".bak.new staging file should be removed");
    }

    // ----- load_json_file_strict -----

    #[test]
    fn strict_load_returns_empty_for_missing_file() {
        let dir = tmpdir();
        let path = dir.path().join("nope.json");
        let val = load_json_file_strict(&path).unwrap();
        assert_eq!(val, serde_json::json!({}));
    }

    #[test]
    fn strict_load_returns_empty_for_blank_file() {
        let dir = tmpdir();
        let path = dir.path().join("empty.json");
        fs::write(&path, "   \n  ").unwrap();
        let val = load_json_file_strict(&path).unwrap();
        assert_eq!(val, serde_json::json!({}));
    }

    #[test]
    fn strict_load_parses_valid_json() {
        let dir = tmpdir();
        let path = dir.path().join("valid.json");
        fs::write(&path, r#"{"hello": "world", "n": 42}"#).unwrap();
        let val = load_json_file_strict(&path).unwrap();
        assert_eq!(val["hello"], "world");
        assert_eq!(val["n"], 42);
    }

    #[test]
    fn strict_load_errors_on_invalid_json() {
        let dir = tmpdir();
        let path = dir.path().join("bad.json");
        fs::write(&path, "not json {{{").unwrap();
        let err = load_json_file_strict(&path).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("cannot parse"), "error: {msg}");
        assert!(
            msg.contains("bad.json"),
            "error should mention filename: {msg}"
        );
    }

    #[test]
    fn strict_load_errors_on_truncated_json() {
        let dir = tmpdir();
        let path = dir.path().join("trunc.json");
        fs::write(&path, r#"{"key": "value", "incomplete"#).unwrap();
        assert!(load_json_file_strict(&path).is_err());
    }

    // ----- load_jsonc_file_strict -----

    #[test]
    fn strict_jsonc_load_returns_empty_for_missing() {
        let dir = tmpdir();
        let path = dir.path().join("nope.jsonc");
        let val = load_jsonc_file_strict(&path).unwrap();
        assert_eq!(val, serde_json::json!({}));
    }

    #[test]
    fn strict_jsonc_load_parses_valid_jsonc() {
        let dir = tmpdir();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            "{\n  // comment\n  \"key\": \"val\",\n  /* block */ \"n\": 1,\n}",
        )
        .unwrap();
        let val = load_jsonc_file_strict(&path).unwrap();
        assert_eq!(val["key"], "val");
        assert_eq!(val["n"], 1);
    }

    #[test]
    fn strict_jsonc_load_errors_on_garbage() {
        let dir = tmpdir();
        let path = dir.path().join("garbage.json");
        fs::write(&path, "totally not json or jsonc !!!").unwrap();
        let err = load_jsonc_file_strict(&path).unwrap_err();
        assert!(err.to_string().contains("cannot parse"));
    }

    // ----- safe_write_json_file -----

    #[test]
    fn safe_write_creates_file_from_scratch() {
        let dir = tmpdir();
        let path = dir.path().join("new.json");
        let value = serde_json::json!({"created": true});
        safe_write_json_file(&path, &value, None).unwrap();

        let written = fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&written).unwrap();
        assert_eq!(parsed["created"], true);
    }

    #[test]
    fn safe_write_replaces_existing_file_atomically() {
        let dir = tmpdir();
        let path = dir.path().join("existing.json");
        fs::write(&path, r#"{"old": true}"#).unwrap();

        let value = serde_json::json!({"new": true});
        safe_write_json_file(&path, &value, None).unwrap();

        let parsed: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed["new"], true);
        assert!(parsed.get("old").is_none());
    }

    #[test]
    fn safe_write_cleans_up_new_file_on_success() {
        let dir = tmpdir();
        let path = dir.path().join("config.json");
        safe_write_json_file(&path, &serde_json::json!({}), None).unwrap();

        let new_path = dir.path().join("config.json.new");
        assert!(!new_path.exists(), ".new staging file should be removed");
    }

    #[test]
    fn safe_write_creates_parent_dirs() {
        let dir = tmpdir();
        let path = dir.path().join("deep").join("nested").join("config.json");
        safe_write_json_file(&path, &serde_json::json!({"deep": true}), None).unwrap();
        assert!(path.exists());
    }

    // ----- write_json_file (convenience wrapper) -----

    #[test]
    fn write_json_file_creates_backup_automatically() {
        let dir = tmpdir();
        let path = dir.path().join("auto.json");
        fs::write(&path, r#"{"original": true}"#).unwrap();

        write_json_file(&path, &serde_json::json!({"updated": true})).unwrap();

        // .bak should exist with original content
        let bak = dir.path().join("auto.json.bak");
        assert!(bak.exists());
        let backup_content: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&bak).unwrap()).unwrap();
        assert_eq!(backup_content["original"], true);
    }

    // ----- THE KEY REGRESSION TEST -----
    // This is the exact bug the fix addresses: load_json_file silently
    // returned {} on parse failure, and the install wrote {} + tracedecay
    // back, destroying the user's config.

    #[test]
    fn invalid_json_is_never_silently_replaced() {
        let dir = tmpdir();
        let path = dir.path().join("opencode.json");
        // Simulate a file that serde_json can't parse (e.g. has trailing commas
        // that the non-strict loader would silently drop).
        let corrupted =
            r#"{"mcp": {"other_server": {"url": "http://example.com"},}, "theme": "dark",}"#;
        fs::write(&path, corrupted).unwrap();

        // The strict loader must refuse to parse this.
        let err = load_json_file_strict(&path);
        assert!(err.is_err(), "strict loader must reject invalid JSON");

        // The original file must be completely untouched.
        assert_eq!(fs::read_to_string(&path).unwrap(), corrupted);

        // Contrast: the old non-strict loader silently returns {} — this
        // is the exact behavior that destroyed configs.
        let old_style = load_json_file(&path);
        assert_eq!(
            old_style,
            serde_json::json!({}),
            "non-strict loader returns empty"
        );
    }

    #[test]
    fn full_install_cycle_preserves_existing_config() {
        // Simulate the full install cycle: backup → strict load → mutate → safe write.
        // Existing keys must be preserved.
        let dir = tmpdir();
        let path = dir.path().join("config.json");
        let original = serde_json::json!({
            "theme": "dark",
            "mcp": {
                "existing_server": {"url": "http://localhost:8080"}
            },
            "other_setting": [1, 2, 3]
        });
        fs::write(&path, serde_json::to_string_pretty(&original).unwrap()).unwrap();

        // Simulate install
        let backup = backup_config_file(&path).unwrap();
        let mut config = load_json_file_strict(&path).unwrap();
        config["mcp"]["tracedecay"] = serde_json::json!({
            "type": "local",
            "command": ["tracedecay", "serve"]
        });
        safe_write_json_file(&path, &config, backup.as_deref()).unwrap();

        // Verify
        let result: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        // TraceDecay was added
        assert!(result["mcp"]["tracedecay"].is_object());
        // Existing keys survived
        assert_eq!(result["theme"], "dark");
        assert_eq!(
            result["mcp"]["existing_server"]["url"],
            "http://localhost:8080"
        );
        assert_eq!(result["other_setting"], serde_json::json!([1, 2, 3]));

        // Backup exists with original content
        let bak_content: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(backup.unwrap()).unwrap()).unwrap();
        assert!(bak_content.get("tracedecay").is_none());
        assert_eq!(bak_content["theme"], "dark");
    }

    #[test]
    fn full_install_cycle_aborts_on_corrupt_file() {
        // If the existing config is corrupt, the install must fail without
        // touching the file. This is the core regression test.
        let dir = tmpdir();
        let path = dir.path().join("config.json");
        let corrupt_content = "{ this is not valid json at all }}}";
        fs::write(&path, corrupt_content).unwrap();

        // Backup succeeds (it just copies bytes)
        let backup = backup_config_file(&path).unwrap();
        assert!(backup.is_some());

        // Strict load fails
        let err = load_json_file_strict(&path);
        assert!(err.is_err());

        // Original file is byte-for-byte unchanged
        assert_eq!(fs::read_to_string(&path).unwrap(), corrupt_content);
        // Backup also has the same content
        assert_eq!(
            fs::read_to_string(backup.unwrap()).unwrap(),
            corrupt_content
        );
    }

    #[test]
    fn safe_write_output_is_valid_json() {
        // Verify the written file is always parseable JSON (round-trip).
        let dir = tmpdir();
        let path = dir.path().join("roundtrip.json");
        let value = serde_json::json!({
            "unicode": "héllo wörld 🦀",
            "nested": {"deep": {"array": [1, null, true, "str"]}},
            "empty_obj": {},
            "empty_arr": []
        });

        safe_write_json_file(&path, &value, None).unwrap();

        let raw = fs::read_to_string(&path).unwrap();
        let reparsed: serde_json::Value =
            serde_json::from_str(&raw).expect("written file must be valid JSON");
        assert_eq!(reparsed, value);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod path_normalize_tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn path_lookup_preserves_non_unicode_parent_components() {
        use std::os::unix::ffi::OsStringExt;

        let dir = tempfile::tempdir().unwrap();
        let invalid_parent = dir
            .path()
            .join(std::ffi::OsString::from_vec(vec![b'n', b'o', b'n', 0xff]));
        let executable = invalid_parent.join(tracedecay_bin_name());
        std::fs::create_dir_all(&invalid_parent).unwrap();
        std::fs::write(&executable, "").unwrap();
        let path_var = std::env::join_paths([&invalid_parent]).unwrap();

        assert_eq!(
            which_tracedecay_path_from(None, Some(path_var.as_os_str()), None),
            Some(executable)
        );
        assert!(which_tracedecay_from(None, Some(path_var.as_os_str()), None).is_none());
    }

    #[test]
    fn path_lookup_absolutizes_relative_path_entries() {
        let relative = Path::new("target").join(tracedecay_bin_name());

        let absolute = absolute_executable_path(&relative).expect("absolute path");

        assert!(absolute.is_absolute());
        assert!(absolute.ends_with(&relative));
    }

    #[cfg(windows)]
    #[test]
    fn windows_path_classification_is_case_insensitive() {
        assert!(is_tracedecay_exe(Path::new(r"C:\Tools\TraceDecay.EXE")));
        assert!(is_cargo_target_binary(
            Path::new(r"C:\Work\TARGET\DEBUG\TraceDecay.EXE"),
            None
        ));
        assert!(is_cargo_target_binary(
            Path::new(r"C:\Work\Custom\TraceDecay.EXE"),
            Some(Path::new(r"c:\work\CUSTOM"))
        ));
    }

    #[test]
    fn normalizes_windows_backslashes() {
        assert_eq!(
            normalize_path_separators(r"C:\Users\dev\scoop\shims\tracedecay.exe"),
            "C:/Users/dev/scoop/shims/tracedecay.exe"
        );
    }

    #[test]
    fn leaves_unix_paths_unchanged() {
        assert_eq!(
            normalize_path_separators("/usr/local/bin/tracedecay"),
            "/usr/local/bin/tracedecay"
        );
    }

    #[test]
    fn which_tracedecay_prefers_path_when_current_exe_is_cargo_target_binary() {
        let dir = tempfile::tempdir().unwrap();
        let path_bin = dir.path().join("bin").join(tracedecay_bin_name());
        std::fs::create_dir_all(path_bin.parent().unwrap()).unwrap();
        std::fs::write(&path_bin, "").unwrap();
        let current_exe = dir
            .path()
            .join("checkout/target/debug")
            .join(tracedecay_bin_name());
        let path_var = std::env::join_paths([dir.path().join("bin")]).unwrap();

        let found = which_tracedecay_from(Some(&current_exe), Some(path_var.as_os_str()), None)
            .expect("PATH binary should be preferred over cargo target binary");

        assert_eq!(
            found,
            normalize_path_separators(&path_bin.to_string_lossy())
        );
    }

    #[test]
    fn which_tracedecay_prefers_path_when_current_exe_is_custom_cargo_target_binary() {
        let dir = tempfile::tempdir().unwrap();
        let path_bin = dir.path().join("bin").join(tracedecay_bin_name());
        std::fs::create_dir_all(path_bin.parent().unwrap()).unwrap();
        std::fs::write(&path_bin, "").unwrap();
        let cargo_target_dir = dir.path().join("custom-target");
        let current_exe = cargo_target_dir.join("debug").join(tracedecay_bin_name());
        let path_var = std::env::join_paths([dir.path().join("bin")]).unwrap();

        let found = which_tracedecay_from(
            Some(&current_exe),
            Some(path_var.as_os_str()),
            Some(&cargo_target_dir),
        )
        .expect("PATH binary should be preferred over a custom cargo target binary");

        assert_eq!(
            found,
            normalize_path_separators(&path_bin.to_string_lossy())
        );
    }

    #[test]
    fn which_tracedecay_skips_cargo_target_binary_on_path() {
        let dir = tempfile::tempdir().unwrap();
        let target_bin = dir
            .path()
            .join("checkout/target/debug")
            .join(tracedecay_bin_name());
        let stable_bin = dir.path().join(".cargo/bin").join(tracedecay_bin_name());
        std::fs::create_dir_all(target_bin.parent().unwrap()).unwrap();
        std::fs::create_dir_all(stable_bin.parent().unwrap()).unwrap();
        std::fs::write(&target_bin, "").unwrap();
        std::fs::write(&stable_bin, "").unwrap();
        let path_var =
            std::env::join_paths([target_bin.parent().unwrap(), stable_bin.parent().unwrap()])
                .unwrap();

        let found = which_tracedecay_from(None, Some(path_var.as_os_str()), None)
            .expect("stable PATH binary should be found after skipping cargo target binary");

        assert_eq!(
            found,
            normalize_path_separators(&stable_bin.to_string_lossy())
        );
    }

    #[test]
    fn which_tracedecay_keeps_non_target_current_exe() {
        let dir = tempfile::tempdir().unwrap();
        let path_bin = dir.path().join("bin").join(tracedecay_bin_name());
        std::fs::create_dir_all(path_bin.parent().unwrap()).unwrap();
        std::fs::write(&path_bin, "").unwrap();
        let current_exe = dir.path().join(".cargo/bin").join(tracedecay_bin_name());
        let path_var = std::env::join_paths([dir.path().join("bin")]).unwrap();

        let found = which_tracedecay_from(Some(&current_exe), Some(path_var.as_os_str()), None)
            .expect("non-target current exe should be accepted");

        assert_eq!(
            found,
            normalize_path_separators(&current_exe.to_string_lossy())
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod local_install_safety_tests {
    use super::*;

    #[test]
    fn windows_hook_command_quotes_windows_paths_with_spaces() {
        let command = hook_command_for_platform(
            r"C:\Program Files\tracedecay\tracedecay.exe",
            "hook-test",
            true,
        );

        assert_eq!(
            command,
            r#""C:/Program Files/tracedecay/tracedecay.exe" hook-test"#
        );
    }

    #[test]
    fn posix_hook_command_keeps_single_quote_escaping() {
        let command = hook_command_for_platform("/tmp/tracedecay's/bin", "hook-test", false);

        assert_eq!(command, "'/tmp/tracedecay'\\''s/bin' hook-test");
    }

    #[test]
    fn post_commit_snippet_quotes_posix_binary_paths_with_spaces() {
        let snippet = post_commit_snippet("/tmp/bin with spaces/tracedecay");

        assert!(snippet.contains("'/tmp/bin with spaces/tracedecay' sync"));
    }

    #[cfg(unix)]
    #[test]
    fn project_local_safe_path_rejects_symlinked_target() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        let outside = dir.path().join("outside.md");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(&outside, "outside").unwrap();
        symlink(&outside, project.join("AGENTS.md")).unwrap();

        let err = ensure_project_local_safe_path(&project, &project.join("AGENTS.md")).unwrap_err();
        assert!(
            err.to_string().contains("symlink"),
            "error should clearly identify the symlink risk: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn project_local_safe_path_rejects_symlinked_parent() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        symlink(&outside, project.join(".codex")).unwrap();

        let err = ensure_project_local_safe_path(&project, &project.join(".codex/config.toml"))
            .unwrap_err();
        assert!(
            err.to_string().contains("symlink"),
            "error should clearly identify the symlink risk: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn project_local_safe_path_allows_new_file_under_canonicalized_project_alias() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let actual = dir.path().join("actual");
        let alias = dir.path().join("alias");
        let project = actual.join("project");
        std::fs::create_dir_all(&project).unwrap();
        symlink(&actual, &alias).unwrap();

        let alias_project = alias.join("project");
        ensure_project_local_safe_path(&alias_project, &alias_project.join(".codex/config.toml"))
            .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn project_local_safe_path_reports_symlink_under_canonicalized_project_alias() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let actual = dir.path().join("actual");
        let alias = dir.path().join("alias");
        let project = actual.join("project");
        let outside = dir.path().join("outside.md");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(&outside, "outside").unwrap();
        symlink(&actual, &alias).unwrap();
        symlink(&outside, project.join("AGENTS.md")).unwrap();

        let alias_project = alias.join("project");
        let err = ensure_project_local_safe_path(&alias_project, &alias_project.join("AGENTS.md"))
            .unwrap_err();
        assert!(
            err.to_string().contains("symlink"),
            "error should clearly identify the symlink risk: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn shared_host_writer_refuses_symlinked_config_for_every_integration() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("operator-config.json");
        let config = dir.path().join("host-config.json");
        std::fs::write(&outside, b"operator bytes").unwrap();
        symlink(&outside, &config).unwrap();

        let error = safe_write_bytes_file(&config, b"tracedecay bytes", None).unwrap_err();
        assert!(
            error.to_string().contains("unsafe host metadata path"),
            "shared writer must surface its cross-host symlink refusal: {error}"
        );
        assert_eq!(std::fs::read(&outside).unwrap(), b"operator bytes");
    }
}
