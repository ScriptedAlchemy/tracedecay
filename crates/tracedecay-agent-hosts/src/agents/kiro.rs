//! AWS Kiro agent integration.
//!
//! Global MCP registration is driven through Kiro's own registry CLI
//! (`kiro-cli mcp add` / `kiro-cli mcp remove`), which owns
//! `~/.kiro/settings/mcp.json`. TraceDecay does not merge that file itself: the
//! host owns the registry, and emulating its writes is exactly what the
//! host-capability doctrine forbids. The binary is therefore a hard
//! requirement for the global lifecycle, with no config-editing fallback.
//!
//! The rest of the integration has no CLI equivalent and stays
//! TraceDecay-written: global tracedecay steering
//! (`~/.kiro/steering/tracedecay.md`), a tracedecay-managed Kiro agent
//! (`~/.kiro/agents/tracedecay.json`) selected as the default when doing so
//! does not overwrite a user's existing default-agent choice, and the
//! workspace-local `.kiro/settings/mcp.json`.
//!
//! User-owned Kiro agents remain user-managed. If `~/.kiro/agents/tracedecay.json`
//! already exists and is not the file tracedecay writes, install and uninstall
//! leave it untouched.

use std::io::Write;
use std::ops::Range;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::automation::skill_targets::{
    SkillInstallTarget, install_managed_skills, profile_root_for_agent_home,
};
use crate::errors::{Result, TraceDecayError};

use super::{
    AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext, UpdatePluginOutcome,
    backup_config_file, config_backup_path, load_json_file, load_json_file_strict,
    safe_write_json_file,
};

/// Kiro agent.
pub struct KiroIntegration;

const PROMPT_MARKER: &str = "## TraceDecay: mandatory tool routing";
/// Heading an older tracedecay version wrote for the same steering block. An
/// existing install carries this marker (with the same [`PROMPT_END_MARKER`]),
/// so install/uninstall/doctor must recognize it too — otherwise a reinstall
/// appends the new block and strands the old one (duplicate steering), and
/// uninstall never removes it.
const PROMPT_MARKER_LEGACY: &str = "## Prefer tracedecay MCP tools";
const PROMPT_END_MARKER: &str = "<!-- tracedecay:kiro:end -->";
const KIRO_AGENT_NAME: &str = "tracedecay";
const OWNED_AGENT_DESCRIPTION: &str =
    "Default Kiro agent with tracedecay MCP tools and code-research guardrails.";
const KIRO_AGENT_ALL_TOOLS: &str = "*";
const KIRO_ALLOWED_BUILTIN_TOOLS: &str = "@builtin";
const KIRO_ALLOWED_TRACEDECAY_TOOLS: &str = "@tracedecay";
const KIRO_PROMPT_HOOK: &str = "hook-kiro-prompt-submit";

/// Name of Kiro's own MCP registry binary.
const KIRO_CLI: &str = "kiro-cli";

/// What the binary is required *for*, used in the typed absence error.
const KIRO_CLI_LIFECYCLE: &str = "kiro MCP registry lifecycle";

/// Name Kiro's registry selects the server by (`kiro-cli mcp add --name`,
/// `kiro-cli mcp remove --name`) and the key it lands under in
/// `mcpServers`. The two are the same string by Kiro's own contract, so the
/// doctor and registration-state readers below keep reading `mcpServers`.
const KIRO_MCP_SERVER_NAME: &str = "tracedecay";

/// Arguments the tracedecay MCP server is launched with.
///
/// Shared by the CLI-driven global registration (one raw `--args` value per
/// item) and the workspace-local config writer, so the two spellings of the
/// same server cannot drift apart.
const MCP_SERVER_ARGS: &[&str] = &["serve"];

/// A hook the managed Kiro agent registers. Kiro's documented hook entry
/// schema is `command` plus an optional `matcher` — nothing else, so no
/// timeout or other tuning field exists to carry here.
struct KiroManagedHook {
    event: &'static str,
    matcher: Option<&'static str>,
    subcommand: &'static str,
}

/// Every managed-agent hook, in registration order. The single source of
/// truth for the generated agent config ([`managed_agent_config`]) and the
/// doctor checks.
///
/// No `stop`/session-end hook is registered. Kiro's documentation describes a
/// Stop trigger, so the host-event catalog carries it
/// (`fixtures/host_events/kiro.json`, identity `stop`) — but only at
/// `support: documented_unverified`, because tracedecay has never captured a
/// real Kiro stop event or verified Kiro's persisted session format. Until a
/// capture verifies it the native decoder rejects the event (see `decode_kiro`
/// and the `kiro_documented_unverified_events_are_rejected_instead_of_emulated`
/// test in `tracedecay-hooks`), `stock_event_support(Kiro, SessionBoundary)` is
/// `Unavailable`, and no CLI subcommand or managed hook is wired. The catalog
/// entry documents the unverified event rather than enabling it; see
/// `docs/KIRO-INTEGRATION.md` ("Deliberate non-defaults").
const KIRO_MANAGED_HOOKS: &[KiroManagedHook] = &[KiroManagedHook {
    event: "userPromptSubmit",
    matcher: None,
    subcommand: KIRO_PROMPT_HOOK,
}];

/// Builds the managed agent's `hooks` object from [`KIRO_MANAGED_HOOKS`],
/// grouping entries per event in table order. Entries carry exactly Kiro's
/// documented fields (`command`, optional `matcher`); an undocumented field
/// would ship schema noise Kiro never reads.
fn managed_agent_hooks(tracedecay_bin: &str) -> serde_json::Value {
    let mut grouped: Vec<(&str, Vec<serde_json::Value>)> = Vec::new();
    for hook in KIRO_MANAGED_HOOKS {
        let mut entry = json!({
            "command": super::hook_command(tracedecay_bin, hook.subcommand),
        });
        if let Some(matcher) = hook.matcher {
            entry["matcher"] = json!(matcher);
        }
        match grouped.iter_mut().find(|(event, _)| *event == hook.event) {
            Some((_, entries)) => entries.push(entry),
            None => grouped.push((hook.event, vec![entry])),
        }
    }
    let mut events = serde_json::Map::new();
    for (event, entries) in grouped {
        events.insert(event.to_string(), serde_json::Value::Array(entries));
    }
    serde_json::Value::Object(events)
}

fn kiro_home(home: &Path) -> PathBuf {
    // Kiro's registry CLI is invoked with an environment-cleared child and
    // therefore resolves its profile from the admitted HOME. Do the same for
    // every path we inspect or write here; an ambient operator KIRO_HOME must
    // never redirect an isolated lifecycle to another profile.
    home.join(".kiro")
}

fn mcp_config_path(home: &Path) -> PathBuf {
    kiro_home(home).join("settings/mcp.json")
}

fn cli_config_path(home: &Path) -> PathBuf {
    kiro_home(home).join("settings/cli.json")
}

fn managed_agent_path(home: &Path) -> PathBuf {
    kiro_home(home).join("agents/tracedecay.json")
}

fn steering_path(home: &Path) -> PathBuf {
    kiro_home(home).join("steering/tracedecay.md")
}

fn managed_skill_index_path(home: &Path) -> PathBuf {
    kiro_home(home).join("steering/tracedecay-managed-skills.md")
}

fn workspace_mcp_config_path(project_path: &Path) -> PathBuf {
    project_path.join(".kiro/settings/mcp.json")
}

enum KiroDoctorInstallationState {
    HostAbsent,
    TraceDecayAbsent,
    Installed,
}

fn kiro_doctor_installation_state(home: &Path) -> Result<KiroDoctorInstallationState> {
    let host_home = kiro_home(home);
    match std::fs::metadata(&host_home) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            return Err(TraceDecayError::Config {
                message: format!("Kiro home {} is not a directory", host_home.display()),
            });
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(KiroDoctorInstallationState::HostAbsent);
        }
        Err(error) => {
            return Err(TraceDecayError::Config {
                message: format!(
                    "failed to inspect Kiro home {}: {error}",
                    host_home.display()
                ),
            });
        }
    }

    let mcp_path = mcp_config_path(home);
    match std::fs::metadata(&mcp_path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => {
            return Err(TraceDecayError::Config {
                message: format!("Kiro MCP config {} is not a file", mcp_path.display()),
            });
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(KiroDoctorInstallationState::TraceDecayAbsent);
        }
        Err(error) => {
            return Err(TraceDecayError::Config {
                message: format!(
                    "failed to inspect Kiro MCP config {}: {error}",
                    mcp_path.display()
                ),
            });
        }
    }

    let contents = std::fs::read_to_string(&mcp_path).map_err(|error| TraceDecayError::Config {
        message: format!(
            "failed to read Kiro MCP config {}: {error}",
            mcp_path.display()
        ),
    })?;
    if contents.trim().is_empty() {
        return Err(TraceDecayError::Config {
            message: format!("Kiro MCP config {} is empty", mcp_path.display()),
        });
    }
    let config: serde_json::Value =
        serde_json::from_str(&contents).map_err(|error| TraceDecayError::Config {
            message: format!(
                "failed to parse Kiro MCP config {}: {error}",
                mcp_path.display()
            ),
        })?;
    let Some(config) = config.as_object() else {
        return Err(TraceDecayError::Config {
            message: format!(
                "Kiro MCP config {} is not a JSON object",
                mcp_path.display()
            ),
        });
    };
    let Some(servers) = config.get("mcpServers") else {
        return Ok(KiroDoctorInstallationState::TraceDecayAbsent);
    };
    let Some(servers) = servers.as_object() else {
        return Err(TraceDecayError::Config {
            message: format!(
                "Kiro MCP config {} has a non-object mcpServers value",
                mcp_path.display()
            ),
        });
    };
    if servers.contains_key(KIRO_MCP_SERVER_NAME) {
        Ok(KiroDoctorInstallationState::Installed)
    } else {
        Ok(KiroDoctorInstallationState::TraceDecayAbsent)
    }
}

impl AgentIntegration for KiroIntegration {
    fn name(&self) -> &'static str {
        "Kiro"
    }

    fn id(&self) -> &'static str {
        "kiro"
    }

    fn supports_local_install(&self) -> bool {
        true
    }

    /// Workspace-local registration still writes `.kiro/settings/mcp.json`
    /// directly rather than driving `kiro-cli mcp add --scope workspace`.
    /// `--scope workspace` resolves against the CLI's *working directory*, and
    /// `host_cli::run_host_cli` admits the profile home as its working
    /// directory. That cannot target an arbitrary `project_path` from here;
    /// adopting it needs a project-aware host-CLI invocation first. Until then
    /// the file write is the only way to target the requested project. The
    /// global path above *is* CLI-driven.
    fn activate_project_host_component_registration(
        &self,
        _components: &[super::host_bundle_v2::HostBundleComponentV1],
        ctx: &InstallContext,
        project_path: &Path,
    ) -> Result<()> {
        let mcp_path = workspace_mcp_config_path(project_path);
        let steering = project_path.join(".kiro/steering/tracedecay.md");
        let agent_path = project_path.join(".kiro/agents/tracedecay.json");
        let skill_index_path = project_path.join(".kiro/steering/tracedecay-managed-skills.md");
        super::ensure_project_local_safe_paths(
            project_path,
            [
                mcp_path.as_path(),
                steering.as_path(),
                agent_path.as_path(),
                skill_index_path.as_path(),
            ],
        )?;
        install_mcp_server(&mcp_path, &ctx.tracedecay_bin)?;
        install_steering_rules(&steering)?;
        install_managed_agent(
            &agent_path,
            &ctx.tracedecay_bin,
            &steering,
            &ctx.home,
            Some(&skill_index_path),
        )?;
        Ok(())
    }

    fn project_host_component_registration_paths(
        &self,
        _components: &[super::host_bundle_v2::HostBundleComponentV1],
        _home: &Path,
        project_path: &Path,
    ) -> Result<Vec<PathBuf>> {
        Ok(vec![
            workspace_mcp_config_path(project_path),
            project_path.join(".kiro/steering/tracedecay.md"),
            project_path.join(".kiro/agents/tracedecay.json"),
            project_path.join(".kiro/steering/tracedecay-managed-skills.md"),
        ])
    }

    /// Mirrors `activate_project_host_component_registration`: the workspace
    /// scope is file-written for the same working-directory reason.
    fn deactivate_project_host_component_registration(
        &self,
        _components: &[super::host_bundle_v2::HostBundleComponentV1],
        ctx: &InstallContext,
        project_path: &Path,
    ) -> Result<()> {
        let mcp_path = workspace_mcp_config_path(project_path);
        let steering = project_path.join(".kiro/steering/tracedecay.md");
        let agent_path = project_path.join(".kiro/agents/tracedecay.json");
        let skill_index_path = project_path.join(".kiro/steering/tracedecay-managed-skills.md");
        super::ensure_project_local_safe_paths(
            project_path,
            [
                mcp_path.as_path(),
                steering.as_path(),
                agent_path.as_path(),
                skill_index_path.as_path(),
            ],
        )?;
        uninstall_mcp_server(&mcp_path)?;
        remove_steering_rules(&steering);
        remove_kiro_managed_skill_index(&ctx.home, &skill_index_path)?;
        uninstall_managed_agent(&agent_path);
        Ok(())
    }

    fn update_plugin(&self, ctx: &InstallContext) -> Result<UpdatePluginOutcome> {
        // The managed agent file is the only generated artifact (it bakes the
        // tracedecay binary path into its hook commands). The shared MCP
        // config, CLI default-agent setting, and steering rules are config —
        // they stay untouched. A user-managed agent file is never rewritten.
        let agent_path = managed_agent_path(&ctx.home);
        if !is_owned_agent_file(&agent_path) {
            return Ok(UpdatePluginOutcome::NotInstalled);
        }
        let skill_index_path = managed_skill_index_path(&ctx.home);
        install_managed_agent(
            &agent_path,
            &ctx.tracedecay_bin,
            &steering_path(&ctx.home),
            &ctx.home,
            Some(&skill_index_path),
        )?;
        Ok(UpdatePluginOutcome::Refreshed(vec![agent_path]))
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mKiro integration\x1b[0m");
        let host_home = kiro_home(&ctx.home);
        match kiro_doctor_installation_state(&ctx.home) {
            Ok(KiroDoctorInstallationState::HostAbsent) => {
                dc.warn(&format!(
                    "Kiro is not detected at {} — run `tracedecay install --agent kiro` if you use Kiro",
                    host_home.display()
                ));
                return;
            }
            Ok(KiroDoctorInstallationState::TraceDecayAbsent) => {
                dc.warn(&format!(
                    "Kiro is detected at {}, but TraceDecay is not installed — run `tracedecay install --agent kiro` if you use Kiro",
                    host_home.display()
                ));
                return;
            }
            Ok(KiroDoctorInstallationState::Installed) => {}
            Err(error) => {
                dc.fail(&format!("Kiro installation state is unreadable: {error}"));
                return;
            }
        }
        let global_server = doctor_check_mcp_config(dc, &ctx.home);
        doctor_check_workspace_mcp_override(
            dc,
            &ctx.home,
            &ctx.project_path,
            global_server.as_ref(),
        );
        doctor_check_steering(dc, &ctx.home);
        doctor_check_managed_agent(dc, &ctx.home);
        doctor_check_default_agent(dc, &ctx.home);
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

        if component != HostBundleComponentV1::ContextMcp {
            return State::Missing;
        }
        kiro_context_mcp_registration_state(&ctx.home)
    }

    fn is_detected(&self, home: &Path) -> bool {
        kiro_home(home).is_dir()
    }

    fn primary_config_path(&self, home: &Path) -> Option<PathBuf> {
        Some(mcp_config_path(home))
    }

    fn host_registration_paths(&self, home: &Path) -> Vec<PathBuf> {
        vec![
            mcp_config_path(home),
            cli_config_path(home),
            managed_agent_path(home),
            steering_path(home),
            managed_skill_index_path(home),
        ]
    }

    fn host_component_registration_paths(
        &self,
        components: &[super::host_bundle_v2::HostBundleComponentV1],
        home: &Path,
    ) -> Vec<PathBuf> {
        if components == [super::host_bundle_v2::HostBundleComponentV1::ContextMcp] {
            let path = mcp_config_path(home);
            vec![path.clone(), config_backup_path(&path)]
        } else {
            self.host_registration_paths(home)
        }
    }

    fn activate_deployed_host_component_registration(
        &self,
        components: &[super::host_bundle_v2::HostBundleComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        if components.contains(&super::host_bundle_v2::HostBundleComponentV1::ContextMcp) {
            let kiro_cli = require_kiro_cli()?;
            kiro_mcp_add_with(&kiro_cli, &ctx.home, &ctx.tracedecay_bin)?;
        }
        Ok(())
    }

    fn deactivate_deployed_host_component_registration(
        &self,
        components: &[super::host_bundle_v2::HostBundleComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        if components.contains(&super::host_bundle_v2::HostBundleComponentV1::ContextMcp) {
            let kiro_cli = require_kiro_cli()?;
            kiro_mcp_remove_with(&kiro_cli, &ctx.home)?;
        }
        Ok(())
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        mcp_registry_has_tracedecay(&mcp_config_path(home))
    }

    fn export_managed_skills(
        &self,
        home: &Path,
        profile_root: &Path,
    ) -> Result<Vec<crate::automation::skill_targets::SkillInstallSummary>> {
        if !self.has_tracedecay(home) {
            return Ok(Vec::new());
        }
        Ok(vec![install_managed_skills(
            profile_root,
            SkillInstallTarget::Kiro,
            &managed_skill_index_path(home),
        )?])
    }

    fn export_managed_skills_local(
        &self,
        project_root: &Path,
        profile_root: &Path,
    ) -> Result<Vec<crate::automation::skill_targets::SkillInstallSummary>> {
        let skill_index_path = project_root.join(".kiro/steering/tracedecay-managed-skills.md");
        if !workspace_mcp_has_tracedecay(project_root) || !skill_index_path.exists() {
            return Ok(Vec::new());
        }
        Ok(vec![install_managed_skills(
            profile_root,
            SkillInstallTarget::Kiro,
            &skill_index_path,
        )?])
    }
}

fn workspace_mcp_has_tracedecay(project_root: &Path) -> bool {
    mcp_registry_has_tracedecay(&workspace_mcp_config_path(project_root))
}

fn mcp_registry_has_tracedecay(path: &Path) -> bool {
    if !path.exists() {
        return false;
    }
    load_json_file(path)
        .get("mcpServers")
        .and_then(|servers| servers.get("tracedecay"))
        .is_some()
}

// ---------------------------------------------------------------------------
// Install helpers
// ---------------------------------------------------------------------------

fn mcp_server_entry(tracedecay_bin: &str) -> serde_json::Value {
    json!({
        "command": tracedecay_bin,
        "args": MCP_SERVER_ARGS,
        "disabled": false
    })
}

/// Resolve Kiro's own registry CLI, or fail with the typed requirement.
///
/// Kiro owns `~/.kiro/settings/mcp.json` through `kiro-cli mcp`. Its CLI is
/// therefore a hard requirement for the global lifecycle, not a preference
/// with a config-editing fallback: emulating those writes is precisely what
/// the host-capability doctrine forbids, and a half-emulated registration is
/// indistinguishable on disk from a corrupt one.
fn require_kiro_cli() -> Result<PathBuf> {
    super::host_cli::require_host_cli(KIRO_CLI, KIRO_CLI_LIFECYCLE)
}

/// Drive Kiro's own registry to add the tracedecay MCP server globally.
///
/// Split from the trait method so tests can supply a fake CLI and an isolated
/// `HOME` without mutating the process environment.
fn kiro_mcp_add_with(kiro_cli: &Path, home: &Path, tracedecay_bin: &str) -> Result<()> {
    // Make the global scope explicit. Kiro's CLI also supports a workspace
    // registry, but this lifecycle owns only the profile-global entry; the
    // workspace (`--scope workspace`) form is deliberately not driven here —
    // see `activate_project_host_component_registration`.
    let mut args = vec![
        "mcp",
        "add",
        "--name",
        KIRO_MCP_SERVER_NAME,
        "--command",
        tracedecay_bin,
    ];
    for server_arg in MCP_SERVER_ARGS {
        args.extend(["--args", server_arg]);
    }
    args.extend(["--scope", "global", "--force"]);
    run_kiro_mcp_step(kiro_cli, &args, home)
}

/// Drive Kiro's own registry to drop the tracedecay MCP server globally.
fn kiro_mcp_remove_with(kiro_cli: &Path, home: &Path) -> Result<()> {
    run_kiro_mcp_step(
        kiro_cli,
        &[
            "mcp",
            "remove",
            "--name",
            KIRO_MCP_SERVER_NAME,
            "--scope",
            "global",
        ],
        home,
    )
}

/// Run one `kiro-cli mcp ...` step, converting a failed invocation into the
/// host's own diagnosis. The peer-server snapshot is a preservation guard:
/// Kiro owns the registry merge, but a buggy/changed host command must not be
/// allowed to silently discard an operator's other MCP servers. The exact
/// post-command bytes are also recorded through the active host transaction so
/// its existing rollback authority can restore the pre-command document when
/// the command fails or a later verification step rejects the effect.
fn run_kiro_mcp_step(kiro_cli: &Path, args: &[&str], home: &Path) -> Result<()> {
    let mcp_path = mcp_config_path(home);
    let (_, peers_before) = read_mcp_config_observation(&mcp_path)?;
    let outcome = super::host_cli::run_host_cli(kiro_cli, args, home)?;
    // Snapshot once after the child exits. The bytes that pass the peer guard
    // are the bytes recorded for rollback; reading again after recording
    // would create a race in which a foreign writer could be absorbed into the
    // transaction's intended state and later overwritten during recovery.
    let (observed_bytes, peers_after) = read_mcp_config_observation(&mcp_path)?;
    if peers_before != peers_after {
        let invocation = if args.is_empty() {
            kiro_cli.display().to_string()
        } else {
            format!("{} {}", kiro_cli.display(), args.join(" "))
        };
        return Err(TraceDecayError::Config {
            message: format!(
                "`{invocation}` changed peer MCP servers in {}; TraceDecay left the host state unaccepted",
                mcp_path.display()
            ),
        });
    }
    crate::agents::record_host_config_observation_bytes(&mcp_path, observed_bytes.as_deref())?;
    if outcome.succeeded() {
        return Ok(());
    }
    Err(TraceDecayError::Config {
        message: outcome.failure_message(),
    })
}

/// Exact registry-document bytes (absent when nothing is registered yet) and
/// the operator-owned peer MCP server entries read from that document.
type McpConfigObservation = (Option<Vec<u8>>, serde_json::Map<String, serde_json::Value>);

fn read_mcp_config_observation(path: &Path) -> Result<McpConfigObservation> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(TraceDecayError::Config {
                message: format!("failed to read {} before Kiro CLI: {error}", path.display()),
            });
        }
    };
    let Some(bytes) = bytes.as_deref() else {
        return Ok((None, serde_json::Map::new()));
    };
    let config = serde_json::from_slice::<serde_json::Value>(bytes).map_err(|error| {
        TraceDecayError::Config {
            message: format!("failed to parse {} as JSON: {error}", path.display()),
        }
    })?;
    let Some(servers) = config.get("mcpServers") else {
        return Ok((Some(bytes.to_vec()), serde_json::Map::new()));
    };
    let Some(servers) = servers.as_object() else {
        return Err(TraceDecayError::Config {
            message: format!("{}.mcpServers must be a JSON object", path.display()),
        });
    };
    let peers = servers
        .iter()
        .filter(|(name, _)| name.as_str() != KIRO_MCP_SERVER_NAME)
        .map(|(name, server)| (name.clone(), server.clone()))
        .collect();
    Ok((Some(bytes.to_vec()), peers))
}

/// Render a path as a `file://` resource URI for Kiro's agent config. Reuses
/// the LSP client's encoder, which additionally handles Windows drive paths and
/// UNC (`//server/share`) prefixes; POSIX paths encode identically to before.
fn file_resource_uri(path: &Path) -> String {
    tracedecay_lsp::analyzer::client::file_uri_from_path_text(&path.to_string_lossy())
}

fn managed_agent_config(
    tracedecay_bin: &str,
    steering_path: &Path,
    managed_skill_index_path: Option<&Path>,
) -> serde_json::Value {
    let mut resources = vec![file_resource_uri(steering_path)];
    if let Some(path) = managed_skill_index_path {
        resources.push(file_resource_uri(path));
    }
    json!({
        "name": KIRO_AGENT_NAME,
        "description": OWNED_AGENT_DESCRIPTION,
        "includeMcpJson": true,
        "resources": resources,
        "tools": [KIRO_AGENT_ALL_TOOLS],
        "allowedTools": [KIRO_ALLOWED_BUILTIN_TOOLS, KIRO_ALLOWED_TRACEDECAY_TOOLS],
        "hooks": managed_agent_hooks(tracedecay_bin)
    })
}

/// Register MCP server in a workspace-local `.kiro/settings/mcp.json`.
fn install_mcp_server(path: &Path, tracedecay_bin: &str) -> Result<()> {
    let backup = backup_config_file(path)?;
    let mut config = match load_json_file_strict(path) {
        Ok(v) => v,
        Err(e) => {
            if let Some(ref b) = backup {
                eprintln!("  Backup preserved at: {}", b.display());
            }
            return Err(e);
        }
    };

    ensure_json_object(&config, path)?;
    ensure_child_object(&mut config, "mcpServers", path)?;
    config["mcpServers"]["tracedecay"] = mcp_server_entry(tracedecay_bin);

    safe_write_json_file(path, &config, backup.as_deref())?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Added tracedecay MCP server to {}",
        path.display()
    );
    Ok(())
}

/// Create or refresh the tracedecay-owned Kiro agent.
///
/// Returns true when tracedecay owns the resulting agent file. A pre-existing
/// user-managed `tracedecay.json` is preserved and returns false so the default
/// agent selector is not pointed at a file whose policy tracedecay does not own.
fn install_managed_agent(
    path: &Path,
    tracedecay_bin: &str,
    steering_path: &Path,
    profile_home: &Path,
    managed_skill_index_path: Option<&Path>,
) -> Result<bool> {
    if path.exists() && !is_owned_agent_file(path) {
        eprintln!(
            "  {} already exists and is user-managed, leaving unchanged",
            path.display()
        );
        return Ok(false);
    }

    let managed_skill_index_path = match managed_skill_index_path {
        Some(index_path) => install_kiro_managed_skill_index(profile_home, index_path)?,
        None => None,
    };
    let backup = backup_config_file(path)?;
    let config = managed_agent_config(tracedecay_bin, steering_path, managed_skill_index_path);
    safe_write_json_file(path, &config, backup.as_deref())?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Wrote tracedecay Kiro agent to {}",
        path.display()
    );
    Ok(true)
}

fn install_kiro_managed_skill_index<'a>(
    home: &Path,
    index_path: &'a Path,
) -> Result<Option<&'a Path>> {
    let profile_root = profile_root_for_agent_home(home);
    super::retired_memory_digest::remove_state(&profile_root)?;
    super::retired_memory_digest::remove_prompt_block(index_path)?;
    let summary = install_managed_skills(&profile_root, SkillInstallTarget::Kiro, index_path)?;
    Ok((summary.exported_count > 0).then_some(index_path))
}

fn remove_kiro_managed_skill_index(home: &Path, index_path: &Path) -> Result<()> {
    super::remove_managed_skill_prompt_index(home, index_path, SkillInstallTarget::Kiro)
}

fn is_builtin_default_agent(agent: &str) -> bool {
    matches!(agent, "kiro_default" | "default")
}

fn ensure_json_object(config: &serde_json::Value, path: &Path) -> Result<()> {
    if config.is_object() {
        Ok(())
    } else {
        Err(TraceDecayError::Config {
            message: format!("{} must contain a JSON object", path.display()),
        })
    }
}

fn ensure_child_object(config: &mut serde_json::Value, key: &str, path: &Path) -> Result<()> {
    if config.get(key).is_none() {
        config[key] = json!({});
        return Ok(());
    }
    if config.get(key).is_some_and(serde_json::Value::is_object) {
        Ok(())
    } else {
        Err(TraceDecayError::Config {
            message: format!("{}.{} must be a JSON object", path.display(), key),
        })
    }
}

/// Add or refresh tracedecay's global steering resource for default Kiro
/// sessions. When the marker is present but the block content is stale (an
/// older tracedecay version wrote it), the block is replaced in place: a
/// marker-to-end-marker splice when the owned end marker exists, otherwise
/// the generic marker-to-next-heading strip plus a fresh append.
fn install_steering_rules(path: &Path) -> Result<()> {
    let existing = if path.exists() {
        std::fs::read_to_string(path).unwrap_or_default()
    } else {
        String::new()
    };
    let block = prompt_rules_text();
    if existing.contains(&block) {
        eprintln!("  Kiro steering already contains tracedecay rules, skipping");
        return Ok(());
    }
    if contains_prompt_marker(&existing) {
        if let Some(range) = tracedecay_prompt_block_range(&existing) {
            let mut new_contents = String::with_capacity(existing.len() + block.len());
            new_contents.push_str(&existing[..range.start]);
            new_contents.push_str(&block);
            new_contents.push_str(&existing[range.end..]);
            std::fs::write(path, new_contents).map_err(|e| TraceDecayError::Config {
                message: format!("failed to write {}: {e}", path.display()),
            })?;
            eprintln!(
                "\x1b[32m✔\x1b[0m Refreshed tracedecay rules in {}",
                path.display()
            );
            return Ok(());
        }
        // Marker present but no owned end marker: fall back to the heading-based
        // strip the other hosts use (trying both the current and legacy
        // heading), then append fresh rules.
        let marker = if existing.contains(PROMPT_MARKER) {
            PROMPT_MARKER
        } else {
            PROMPT_MARKER_LEGACY
        };
        let stripped =
            super::prompt_rules::strip_heading_block(&existing, marker).unwrap_or_default();
        return super::prompt_rules::write_refreshed(path, &stripped, &block);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| TraceDecayError::Config {
            message: format!("failed to create {}: {e}", parent.display()),
        })?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| TraceDecayError::Config {
            message: format!("failed to open {}: {e}", path.display()),
        })?;
    let separator = if existing.trim().is_empty() {
        ""
    } else {
        "\n\n"
    };
    writeln!(f, "{separator}{block}").map_err(|e| TraceDecayError::Config {
        message: format!("failed to write {}: {e}", path.display()),
    })?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Appended tracedecay rules to {}",
        path.display()
    );
    Ok(())
}

fn prompt_rules_text() -> String {
    format!(
        "{}\n\n{}",
        prompt_rules_text_without_end_marker(),
        PROMPT_END_MARKER
    )
}

fn prompt_rules_text_without_end_marker() -> String {
    format!(
        "## TraceDecay: mandatory tool routing\n\n\
This project has a live TraceDecay code graph. If there is even a 1% chance a \
tracedecay tool applies, you MUST use it BEFORE Kiro's file reads, codebase scan, \
or `delegate`. This is a mandate, not a preference — you cannot rationalize your \
way out of it.\n\n\
| The moment you are in | Do this first |\n\
|---|---|\n\
| Searching for literal/regex code text | `tracedecay_grep` |\n\
| Searching for a symbol or concept | `tracedecay_search` (names) or `tracedecay_context` (concepts) |\n\
| About to read a source file | `tracedecay_outline` -> `tracedecay_body` -> `tracedecay_read` slices |\n\
| \"Who calls X\" / \"what does X call\" / \"trace this\" | `tracedecay_callers` / `tracedecay_callees` |\n\
| About to change code, wondering what breaks | `tracedecay_impact` / `tracedecay_diff_context` / `tracedecay_affected` |\n\
| Project / storage identity question | `tracedecay_active_project` / `tracedecay_storage_status` |\n\
| A prior decision or past conversation is referenced | `tracedecay_message_search` / `tracedecay_lcm_expand_query` |\n\n\
| Red-flag thought | Reality |\n\
|---|---|\n\
| \"Grep is faster for this\" | `tracedecay_grep` handles literal/regex code search; `tracedecay_search` is pre-ranked for names. |\n\
| \"I'll just read the whole file\" | `tracedecay_outline` / `tracedecay_body` answer at a fraction of the tokens. |\n\
| \"This is a simple lookup\" | Simple lookups are exactly what the graph is for. |\n\
| \"I already know this codebase\" | The graph is fresher than your memory. Check it. |\n\n\
SUBAGENT-STOP: if you were handed the exact files, symbols, or excerpts to act on, \
do NOT re-run discovery — act on what you were given. Explicit user instructions and \
project rules (CLAUDE.md / AGENTS.md) win over this mandate; the mandate wins over the \
default \"just grep it\" habit. Never fight a direct instruction to satisfy it.\n\n\
Do not use Kiro's `delegate` tool for codebase exploration, architecture mapping, \
call graph work, symbol lookup, or other code research until tracedecay MCP tools \
have been tried. Delegation is still appropriate for long-running execution work \
such as builds, tests, generated reports, or independent implementation tasks.\n\n\
For durable project/user facts, use `tracedecay_fact_store_add` to persist them and \
`tracedecay_fact_store_search` to recall or deduplicate them; use \
`tracedecay_fact_feedback` and read-only `tracedecay_memory_status` over ad-hoc notes. Do not \
store secrets, credentials, or unnecessary PII in persistent facts. Use \
`memory_scope=user` for durable preferences or projectless chat and \
`memory_scope=project` for active-codebase facts.\n\n\
{cli_fallback}\n\n\
If you discover a gap where an extractor, schema, or tracedecay tool could answer a \
question natively, propose opening an issue at \
https://github.com/ScriptedAlchemy/tracedecay. Remind the user to strip sensitive \
or proprietary code from the bug description before submitting.",
        cli_fallback = super::CLI_FALLBACK_PROMPT_RULES,
    )
}

// ---------------------------------------------------------------------------
// Uninstall helpers
// ---------------------------------------------------------------------------

fn uninstall_mcp_server(path: &Path) -> Result<()> {
    if !path.exists() {
        eprintln!("  {} not found, skipping", path.display());
        return Ok(());
    }
    let contents = std::fs::read_to_string(path).map_err(|error| TraceDecayError::Config {
        message: format!("failed to read {}: {error}", path.display()),
    })?;
    let mut config = serde_json::from_str::<serde_json::Value>(&contents).map_err(|error| {
        TraceDecayError::Config {
            message: format!("failed to parse {} as JSON: {error}", path.display()),
        }
    })?;
    let Some(servers) = config.get_mut("mcpServers").and_then(|v| v.as_object_mut()) else {
        eprintln!("  No tracedecay MCP server in {}, skipping", path.display());
        return Ok(());
    };
    let removed = servers.remove("tracedecay").is_some();
    if !removed {
        eprintln!("  No tracedecay MCP server in {}, skipping", path.display());
        return Ok(());
    }
    if servers.is_empty() {
        config.as_object_mut().map(|o| o.remove("mcpServers"));
    }
    let is_empty = config.as_object().is_some_and(serde_json::Map::is_empty);
    if is_empty {
        backup_config_file(path)?;
        super::safe_remove_host_file(path).map_err(|error| TraceDecayError::Config {
            message: format!("failed to remove {}: {error}", path.display()),
        })?;
        tracedecay_private_fs::framed_log::sync_parent_directory(
            path,
            tracedecay_private_fs::framed_log::DirectorySyncPolicy::TolerateUnsupported,
        )
        .map_err(|error| TraceDecayError::Config {
            message: format!("failed to durably remove {}: {error}", path.display()),
        })?;
        eprintln!("\x1b[32m✔\x1b[0m Removed {} (was empty)", path.display());
    } else {
        let backup = backup_config_file(path)?;
        safe_write_json_file(path, &config, backup.as_deref())?;
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed tracedecay MCP server from {}",
            path.display()
        );
    }
    Ok(())
}

fn remove_steering_rules(path: &Path) {
    if !path.exists() {
        return;
    }
    let Ok(contents) = std::fs::read_to_string(path) else {
        return;
    };
    if !contains_prompt_marker(&contents) {
        eprintln!("  Kiro steering does not contain tracedecay rules, skipping");
        return;
    }
    let Some(range) = tracedecay_prompt_block_range(&contents) else {
        eprintln!(
            "  Kiro steering contains tracedecay rules without an owned end marker; leaving unchanged"
        );
        return;
    };
    let mut new_contents = String::new();
    new_contents.push_str(contents[..range.start].trim_end());
    let remainder = &contents[range.end..];
    if !remainder.is_empty() {
        new_contents.push_str("\n\n");
        new_contents.push_str(remainder.trim_start());
    }
    let new_contents = new_contents.trim().to_string();
    if new_contents.is_empty() {
        super::safe_remove_host_file(path).ok();
        eprintln!("\x1b[32m✔\x1b[0m Removed {} (was empty)", path.display());
    } else {
        std::fs::write(path, format!("{new_contents}\n")).ok();
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed tracedecay rules from {}",
            path.display()
        );
    }
}

fn uninstall_managed_agent(path: &Path) {
    if !path.exists() {
        return;
    }
    if !is_owned_agent_file(path) {
        eprintln!("  {} is user-managed, leaving unchanged", path.display());
        return;
    }
    if super::safe_remove_host_file(path).is_ok() {
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed tracedecay Kiro agent from {}",
            path.display()
        );
    }
}

fn is_owned_agent_file(path: &Path) -> bool {
    if !path.exists() {
        return false;
    }
    let config = load_json_file(path);
    is_owned_agent_config(&config)
}

fn is_owned_agent_config(config: &serde_json::Value) -> bool {
    config.get("name").and_then(serde_json::Value::as_str) == Some(KIRO_AGENT_NAME)
        && config
            .get("description")
            .and_then(serde_json::Value::as_str)
            == Some(OWNED_AGENT_DESCRIPTION)
}

fn kiro_context_mcp_registration_state(
    home: &Path,
) -> super::host_bundle_v2::HostBundleRegistrationStateV1 {
    use super::host_bundle_v2::HostBundleRegistrationStateV1 as State;

    let Ok(mcp_bytes) = std::fs::read(mcp_config_path(home)) else {
        return State::Missing;
    };
    let Ok(mcp_config) = serde_json::from_slice::<serde_json::Value>(&mcp_bytes) else {
        return State::Corrupt;
    };
    let Some(server) = mcp_config
        .pointer("/mcpServers/tracedecay")
        .and_then(serde_json::Value::as_object)
    else {
        return State::Missing;
    };
    let mcp_current = server
        .get("command")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|command| !command.is_empty())
        && server
            .get("args")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|args| args.iter().any(|arg| arg.as_str() == Some("serve")))
        && server.get("disabled").and_then(serde_json::Value::as_bool) != Some(true);
    if !mcp_current {
        return State::Repairable;
    }
    State::Current
}

/// True when the steering file carries either the current or the legacy
/// tracedecay block marker.
fn contains_prompt_marker(contents: &str) -> bool {
    contents.contains(PROMPT_MARKER) || contents.contains(PROMPT_MARKER_LEGACY)
}

/// Byte range of the tracedecay steering block, starting at whichever marker
/// (current or legacy) appears first and running to the owned end marker. The
/// legacy block carries the same [`PROMPT_END_MARKER`], so a legacy install is
/// spliced/removed in place exactly like a current one.
fn tracedecay_prompt_block_range(contents: &str) -> Option<Range<usize>> {
    let start = [PROMPT_MARKER, PROMPT_MARKER_LEGACY]
        .iter()
        .filter_map(|marker| contents.find(marker))
        .min()?;
    let marker = PROMPT_END_MARKER;
    let end_marker = contents[start..].find(marker)?;
    let end = start + end_marker + marker.len();
    Some(start..end)
}

// ---------------------------------------------------------------------------
// Healthcheck helpers
// ---------------------------------------------------------------------------

fn doctor_check_mcp_config(dc: &mut DoctorCounters, home: &Path) -> Option<serde_json::Value> {
    let path = mcp_config_path(home);
    if !path.exists() {
        dc.warn(&format!(
            "{} not found -- run `tracedecay install --agent kiro` if you use Kiro",
            path.display()
        ));
        return None;
    }

    let config = load_json_file(&path);
    let server = config.get("mcpServers").and_then(|v| v.get("tracedecay"));

    let Some(server_value) = server else {
        dc.fail(&format!(
            "MCP server NOT registered in {} -- run `tracedecay install --agent kiro`",
            path.display()
        ));
        return None;
    };
    let Some(server) = server_value.as_object() else {
        dc.fail(&format!(
            "MCP server in {} is not an object -- run `tracedecay install --agent kiro`",
            path.display()
        ));
        return None;
    };
    dc.pass(&format!("MCP server registered in {}", path.display()));

    let has_serve = server
        .get("args")
        .and_then(|v| v.as_array())
        .is_some_and(|arr| arr.iter().any(|v| v.as_str() == Some("serve")));
    if has_serve {
        dc.pass("MCP server args include \"serve\"");
    } else {
        dc.fail("MCP server args missing \"serve\" -- run `tracedecay install --agent kiro`");
    }

    let disabled = server
        .get("disabled")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if disabled {
        dc.fail("MCP server is disabled -- run `tracedecay install --agent kiro`");
    } else {
        dc.pass("MCP server is enabled");
    }

    Some(server_value.clone())
}

fn doctor_check_workspace_mcp_override(
    dc: &mut DoctorCounters,
    home: &Path,
    project_path: &Path,
    global_server: Option<&serde_json::Value>,
) {
    let path = workspace_mcp_config_path(project_path);
    if path == mcp_config_path(home) {
        return;
    }
    if !path.exists() {
        dc.pass("No workspace Kiro MCP tracedecay override");
        return;
    }

    let config = load_json_file(&path);
    let server = config.get("mcpServers").and_then(|v| v.get("tracedecay"));
    let Some(server_value) = server else {
        dc.pass("No workspace Kiro MCP tracedecay override");
        return;
    };
    let Some(server) = server_value.as_object() else {
        dc.fail(&format!(
            "Workspace Kiro MCP tracedecay entry in {} is not an object and shadows the global install",
            path.display()
        ));
        return;
    };

    let mut compatible = true;
    let disabled = server
        .get("disabled")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if disabled {
        dc.fail(&format!(
            "Workspace Kiro MCP tracedecay entry in {} is disabled and shadows the global install",
            path.display()
        ));
        compatible = false;
    }

    let has_serve = server
        .get("args")
        .and_then(|v| v.as_array())
        .is_some_and(|arr| arr.iter().any(|v| v.as_str() == Some("serve")));
    if !has_serve {
        dc.fail(&format!(
            "Workspace Kiro MCP tracedecay entry in {} is missing \"serve\" and shadows the global install",
            path.display()
        ));
        compatible = false;
    }

    if let Some(global_server) = global_server {
        let workspace_command = server.get("command").and_then(|v| v.as_str());
        let global_command = global_server.get("command").and_then(|v| v.as_str());
        if workspace_command != global_command {
            dc.fail(&format!(
                "Workspace Kiro MCP tracedecay command in {} differs from the global install",
                path.display()
            ));
            compatible = false;
        }
    }

    if compatible {
        dc.pass(&format!(
            "Workspace Kiro MCP tracedecay override in {} is compatible",
            path.display()
        ));
    }
}

fn doctor_check_steering(dc: &mut DoctorCounters, home: &Path) {
    let path = steering_path(home);
    if !path.exists() {
        dc.warn("~/.kiro/steering/tracedecay.md does not exist");
        return;
    }
    let contents = std::fs::read_to_string(&path).unwrap_or_default();
    if !contains_prompt_marker(&contents) {
        dc.fail(
            "Kiro global tracedecay.md missing tracedecay rules -- run `tracedecay install --agent kiro`",
        );
    } else if tracedecay_prompt_block_range(&contents).is_none() {
        dc.fail(
            "Kiro global tracedecay.md contains tracedecay rules without an owned end marker -- remove the stale block and run `tracedecay install --agent kiro`",
        );
    } else {
        dc.pass("Kiro global tracedecay.md contains tracedecay rules");
    }
}

fn doctor_check_managed_agent(dc: &mut DoctorCounters, home: &Path) {
    let path = managed_agent_path(home);
    if !path.exists() {
        dc.fail(&format!(
            "Kiro tracedecay agent NOT installed at {} -- run `tracedecay install --agent kiro`",
            path.display()
        ));
        return;
    }

    let config = load_json_file(&path);
    if !is_owned_agent_config(&config) {
        dc.warn(&format!(
            "{} is user-managed; tracedecay hooks were not installed there",
            path.display()
        ));
        return;
    }

    dc.pass(&format!("Kiro tracedecay agent: {}", path.display()));

    if config
        .get("includeMcpJson")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        dc.pass("Kiro tracedecay agent includes global/workspace MCP config");
    } else {
        dc.fail("Kiro tracedecay agent missing includeMcpJson=true -- run `tracedecay install --agent kiro`");
    }

    doctor_check_agent_tools(dc, &config);
    doctor_check_agent_allowed_tools(dc, &config);

    let expected_resource = file_resource_uri(&steering_path(home));
    if config
        .get("resources")
        .and_then(|v| v.as_array())
        .is_some_and(|arr| {
            arr.iter()
                .any(|v| v.as_str() == Some(expected_resource.as_str()))
        })
    {
        dc.pass("Kiro tracedecay agent loads global steering as a resource");
    } else {
        dc.fail(
            "Kiro tracedecay agent missing global steering resource -- run `tracedecay install --agent kiro`",
        );
    }

    for hook in KIRO_MANAGED_HOOKS {
        doctor_check_agent_hook(dc, &config, hook.event, hook.matcher, hook.subcommand);
    }
}

fn doctor_check_agent_tools(dc: &mut DoctorCounters, config: &serde_json::Value) {
    if json_array_contains_str(config, "tools", KIRO_AGENT_ALL_TOOLS) {
        dc.pass("Kiro tracedecay agent exposes all configured tools");
    } else {
        dc.warn(
            "Kiro tracedecay agent tools list is not permissive -- run `tracedecay install --agent kiro`",
        );
    }
}

fn doctor_check_agent_allowed_tools(dc: &mut DoctorCounters, config: &serde_json::Value) {
    let required = [KIRO_ALLOWED_BUILTIN_TOOLS, KIRO_ALLOWED_TRACEDECAY_TOOLS];
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|tool| !json_array_contains_str(config, "allowedTools", tool))
        .collect();

    if missing.is_empty() {
        dc.pass("Kiro tracedecay agent pre-approves built-in and tracedecay tools");
    } else {
        dc.warn(
            "Kiro tracedecay agent allowedTools is not permissive -- run `tracedecay install --agent kiro`",
        );
        for tool in missing {
            dc.info(&format!("missing allowedTools entry: {tool}"));
        }
    }
}

fn json_array_contains_str(config: &serde_json::Value, field: &str, expected: &str) -> bool {
    config
        .get(field)
        .and_then(|v| v.as_array())
        .is_some_and(|arr| arr.iter().any(|v| v.as_str() == Some(expected)))
}

fn doctor_check_agent_hook(
    dc: &mut DoctorCounters,
    config: &serde_json::Value,
    event: &str,
    matcher: Option<&str>,
    subcommand: &str,
) {
    let hook = find_agent_hook(config, event, matcher, subcommand);
    let Some(hook) = hook else {
        let matcher_label = matcher.map_or(String::new(), |m| format!(" ({m})"));
        dc.fail(&format!(
            "Kiro {event}{matcher_label} hook missing {subcommand} -- run `tracedecay install --agent kiro`"
        ));
        return;
    };

    // Kiro's hook schema is `command` + optional `matcher` only. A stray
    // `timeout_ms` is residue from an older tracedecay version that wrote an
    // undocumented field; a reinstall rewrites the entry to the exact schema.
    if hook.get("timeout_ms").is_some() {
        dc.warn(&format!(
            "Kiro {event} hook carries an undocumented timeout_ms field from an older \
             tracedecay version -- run `tracedecay install --agent kiro` to rewrite it"
        ));
        return;
    }
    let matcher_label = matcher.map_or(String::new(), |m| format!(" ({m})"));
    dc.pass(&format!("Kiro {event}{matcher_label} hook installed"));
}

fn find_agent_hook<'a>(
    config: &'a serde_json::Value,
    event: &str,
    matcher: Option<&str>,
    subcommand: &str,
) -> Option<&'a serde_json::Value> {
    config
        .get("hooks")
        .and_then(|v| v.get(event))
        .and_then(serde_json::Value::as_array)?
        .iter()
        .find(|hook| {
            let matcher_ok = match matcher {
                Some(expected) => {
                    hook.get("matcher").and_then(serde_json::Value::as_str) == Some(expected)
                }
                None => hook.get("matcher").is_none(),
            };
            matcher_ok
                && hook
                    .get("command")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|cmd| cmd.split_whitespace().any(|part| part == subcommand))
        })
}

fn doctor_check_default_agent(dc: &mut DoctorCounters, home: &Path) {
    let path = cli_config_path(home);
    if !path.exists() {
        dc.fail(&format!(
            "{} not found -- run `tracedecay install --agent kiro`",
            path.display()
        ));
        return;
    }

    let config = load_json_file(&path);
    let default_agent = config
        .get("chat")
        .and_then(|v| v.get("defaultAgent"))
        .and_then(serde_json::Value::as_str);

    match default_agent {
        Some(KIRO_AGENT_NAME) => dc.pass("Kiro default agent is tracedecay"),
        Some(agent) if is_builtin_default_agent(agent) => dc.warn(
            "Kiro default agent is still the built-in default -- run `tracedecay install --agent kiro`",
        ),
        Some(agent) => dc.warn(&format!(
            "Kiro default agent is \"{agent}\"; tracedecay hooks run only when the tracedecay agent is selected"
        )),
        None => dc.warn(
            "Kiro default agent is not set; tracedecay hooks run only when the tracedecay agent is selected",
        ),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
