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

use std::ops::Range;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::errors::{Result, TraceDecayError};
use tracedecay_automation_runtime::automation::skill_targets::{
    SkillInstallTarget, install_managed_skills, profile_root_for_agent_home,
};

use super::{
    AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext, JsonConfigDialect,
    McpUninstallPolicy, UpdatePluginOutcome, backup_config_file, config_backup_path,
    install_mcp_server_entry, load_json_file, mcp_config_has_tracedecay, safe_write_json_file,
    uninstall_mcp_server_entry,
};

pub struct KiroIntegration;

/// Ownership sentinels of the tracedecay steering block. The end sentinel is
/// the one shipped releases already wrote; the start sentinel replaces the
/// heading text as the block's identity so wording can change without another
/// marker migration.
const STEERING_SENTINELS: super::prompt_rules::OwnedBlockSentinels =
    super::prompt_rules::OwnedBlockSentinels {
        start: "<!-- tracedecay:kiro:start -->",
        end: "<!-- tracedecay:kiro:end -->",
    };
/// Heading markers shipped releases (through v0.1.0-beta.37) used as the
/// block's identity. An existing install carries one of them, usually closed by
/// the same end sentinel, so update and uninstall must recognize them —
/// otherwise a reinstall appends the new block and strands the old one, and
/// uninstall never removes it.
const HISTORICAL_STEERING_HEADINGS: [&str; 2] = [
    "## TraceDecay: mandatory tool routing",
    "## Prefer tracedecay MCP tools",
];
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
    #[hotpath::measure(label = "kiro_project_install")]
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
        remove_steering_rules(&steering)?;
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
    ) -> Result<Vec<tracedecay_automation_runtime::automation::skill_targets::SkillInstallSummary>>
    {
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
    ) -> Result<Vec<tracedecay_automation_runtime::automation::skill_targets::SkillInstallSummary>>
    {
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
    mcp_config_has_tracedecay(path, "mcpServers", load_json_file)
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
#[hotpath::measure(label = "kiro_mcp_install")]
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
    run_mcp_registry_step(kiro_cli, &args, home)
}

/// Drive Kiro's own registry to drop the tracedecay MCP server globally.
fn kiro_mcp_remove_with(kiro_cli: &Path, home: &Path) -> Result<()> {
    run_mcp_registry_step(
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

fn run_mcp_registry_step(kiro_cli: &Path, args: &[&str], home: &Path) -> Result<()> {
    super::host_cli::run_mcp_registry_step(
        kiro_cli,
        args,
        home,
        &mcp_config_path(home),
        KIRO_MCP_SERVER_NAME,
        "Kiro CLI",
    )
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
    install_mcp_server_entry(
        path,
        "mcpServers",
        mcp_server_entry(tracedecay_bin),
        "Kiro",
        JsonConfigDialect::Json,
    )
}

/// Create or refresh the tracedecay-owned Kiro agent.
///
/// Returns true when tracedecay owns the resulting agent file. A pre-existing
/// user-managed `tracedecay.json` is preserved and returns false so the default
/// agent selector is not pointed at a file whose policy tracedecay does not own.
#[hotpath::measure(label = "kiro_agent_install")]
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

/// Add or refresh tracedecay's global steering resource for default Kiro
/// sessions. Every owned block — the current sentinel-delimited shape or a
/// historical heading-marked one — converges onto exactly one copy of the
/// current block in place; operator text around it is preserved.
fn install_steering_rules(path: &Path) -> Result<()> {
    let block = steering_block_text();
    super::prompt_rules::reconcile_prompt_rules_with(path, |existing| {
        let ranges = owned_steering_ranges(existing);
        Ok(super::prompt_rules::converge_owned_block(
            existing, &ranges, &block,
        ))
    })
}

fn steering_block_text() -> String {
    STEERING_SENTINELS.render(&steering_guidance_text())
}

fn steering_guidance_text() -> String {
    format!(
        "## TraceDecay code intelligence\n\n\
This project has a TraceDecay code graph exposed as `tracedecay_*` MCP tools. Use it \
when the task is about code structure, callers, impact, or where something lives; use \
Kiro's native file reads and edits for known files, ordinary local edits, and \
non-indexed material.\n\n\
Routing:\n\
- Literal or regex text in code: `tracedecay_grep`.\n\
- A symbol by name: `tracedecay_search`; a concept or \"how does X work\": `tracedecay_context`.\n\
- A source file you have not seen: `tracedecay_outline`, then `tracedecay_body` / \
`tracedecay_read` slices.\n\
- Callers, callees, call chains: `tracedecay_callers` / `tracedecay_callees`.\n\
- What a change breaks: `tracedecay_impact`, `tracedecay_diff_context`, `tracedecay_affected`.\n\
- Project or storage identity: `tracedecay_active_project` / `tracedecay_storage_status`, \
not repo-local marker files or database paths.\n\
- Prior decisions or conversations: `tracedecay_message_search` / `tracedecay_lcm_expand_query`.\n\n\
Read the freshness and coverage line that opens each result; an empty result does not \
prove absence. When you were handed the exact files, symbols, or excerpts to act on, act \
on them instead of re-running discovery. Explicit user instructions and project rules win \
over this guidance.\n\n\
Kiro's `delegate` fits long-running execution such as builds, tests, generated reports, \
and independent implementation; code research is usually answered faster by the graph.\n\n\
For durable project/user facts, `tracedecay_fact_store_add` persists and \
`tracedecay_fact_store_search` recalls or deduplicates them; prefer `tracedecay_fact_feedback` \
and read-only `tracedecay_memory_status` over ad-hoc notes. Use `memory_scope=user` for \
durable preferences or projectless chat and `memory_scope=project` for active-codebase \
facts. Do not store secrets, credentials, or unnecessary PII in persistent facts.\n\n\
{cli_fallback}\n\n\
If an extractor, schema, or tracedecay tool could answer a question natively but does \
not, propose opening an issue at https://github.com/ScriptedAlchemy/tracedecay and remind \
the user to strip sensitive or proprietary code from the description first.",
        cli_fallback = super::CLI_FALLBACK_PROMPT_RULES,
    )
}

// ---------------------------------------------------------------------------
// Uninstall helpers
// ---------------------------------------------------------------------------

fn uninstall_mcp_server(path: &Path) -> Result<()> {
    uninstall_mcp_server_entry(
        path,
        "mcpServers",
        JsonConfigDialect::Json,
        McpUninstallPolicy {
            prune_empty_root: true,
            remove_empty_file: true,
        },
    )
}

/// Remove every tracedecay-owned steering block, current or historical.
fn remove_steering_rules(path: &Path) -> Result<()> {
    super::prompt_rules::remove_prompt_rules_with(path, |contents| {
        let ranges = owned_steering_ranges(contents);
        Ok(super::prompt_rules::remove_owned_blocks(contents, &ranges))
    })
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

/// Every tracedecay-owned steering range in document order: current
/// sentinel-delimited blocks plus historical heading-marked ones.
fn owned_steering_ranges(contents: &str) -> Vec<Range<usize>> {
    super::prompt_rules::owned_block_ranges(contents, first_owned_steering_range)
}

/// Earliest owned block at or after `from`. A historical heading block runs to
/// the shipped end sentinel when that sentinel closes it before any other
/// boundary; otherwise it ends at the next heading, the managed skill index, a
/// current start sentinel, or EOF — the shape the oldest installs wrote.
fn first_owned_steering_range(contents: &str, from: usize) -> Option<Range<usize>> {
    let current = STEERING_SENTINELS.block_range(contents, from);
    let historical = HISTORICAL_STEERING_HEADINGS
        .iter()
        .filter_map(|heading| {
            contents[from..]
                .find(heading)
                .map(|at| (from + at, heading))
        })
        .min_by_key(|(start, _)| *start)
        .map(|(start, heading)| {
            let body_from = start + heading.len();
            let boundary = super::prompt_rules::historical_heading_block_end(
                contents,
                body_from,
                STEERING_SENTINELS,
            );
            let end = contents[body_from..boundary]
                .find(STEERING_SENTINELS.end)
                .map_or(boundary, |at| body_from + at + STEERING_SENTINELS.end.len());
            start..end
        });
    match (current, historical) {
        (Some(current), Some(historical)) if historical.start < current.start => Some(historical),
        (Some(current), _) => Some(current),
        (None, historical) => historical,
    }
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
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) => {
            dc.fail(&format!(
                "Kiro global tracedecay.md is unreadable ({error}) -- run `tracedecay install --agent kiro`"
            ));
            return;
        }
    };
    // Health is judged by the ownership sentinels and byte identity with the
    // embedded block, never by prose inside it.
    let ranges = owned_steering_ranges(&contents);
    if ranges.is_empty() {
        dc.fail(
            "Kiro global tracedecay.md missing tracedecay rules -- run `tracedecay install --agent kiro`",
        );
    } else if super::prompt_rules::owned_block_is_current(
        &contents,
        &ranges,
        &steering_block_text(),
    ) {
        dc.pass("Kiro global tracedecay.md contains current tracedecay rules");
    } else {
        dc.fail(&format!(
            "Kiro global tracedecay.md carries {} outdated or duplicate tracedecay block(s) -- run `tracedecay install --agent kiro` to converge them",
            ranges.len()
        ));
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
