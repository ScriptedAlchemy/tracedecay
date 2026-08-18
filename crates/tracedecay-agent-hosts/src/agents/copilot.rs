//! GitHub Copilot integration.
//!
//! Two independent registration surfaces carry the tracedecay MCP server, and
//! they are owned by different parties:
//!
//! * **Copilot CLI's `~/.copilot/mcp-config.json`** (`mcpServers.tracedecay`)
//!   is owned by GitHub Copilot's own non-interactive registry commands
//!   (`copilot mcp add` / `copilot mcp remove`). TraceDecay drives those
//!   commands and never merges that file itself: the host owns the registry,
//!   and emulating its writes is exactly what the host-capability doctrine
//!   forbids. The `copilot` binary is therefore a **hard requirement** for this
//!   half of the lifecycle, with no config-editing fallback — a half-emulated
//!   registration is indistinguishable on disk from a corrupt one.
//! * **VS Code's `settings.json`** (`mcp.servers.tracedecay`, plus the
//!   Insiders profile) has **no host CLI at all**. VS Code exposes no
//!   non-interactive command that writes an `mcp.servers` entry into a user
//!   settings file, so that half stays TraceDecay-written exactly as it is
//!   today and is only read back here by the doctor. Adopting a host command
//!   for it is not an option that exists; this module must not invent one.
//!
//! The launch arguments both spellings use live in one place
//! ([`MCP_SERVER_ARGS`]) so the CLI-driven registration and the
//! TraceDecay-owned readback cannot drift apart.

use std::path::{Path, PathBuf};

use crate::errors::{Result, TraceDecayError};

use super::{
    AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext, config_backup_path,
    load_json_file, load_jsonc_file,
};

/// Name of GitHub Copilot's own CLI, which owns `~/.copilot/mcp-config.json`.
const COPILOT_CLI: &str = "copilot";

/// What the binary is required *for*, used in the typed absence error so the
/// operator learns both what is missing and which lifecycle needed it.
const COPILOT_CLI_LIFECYCLE: &str = "GitHub Copilot MCP registry lifecycle";

/// Name Copilot's registry selects the server by (`copilot mcp add <name>`,
/// `copilot mcp remove <name>`) and the key it lands under in `mcpServers`.
/// The two are the same string by Copilot's own contract, so the doctor and
/// the peer-preservation guard below keep reading `mcpServers.tracedecay`.
const COPILOT_MCP_SERVER_NAME: &str = "tracedecay";

/// Arguments the tracedecay MCP server is launched with.
///
/// Shared by the CLI-driven registration (the trailing `-- <command> <args…>`
/// words) and by the doctor readback that verifies what actually landed, so
/// the two spellings of the same server cannot drift apart. Any remaining
/// TraceDecay-written registration surface (the VS Code `settings.json` half)
/// must source its `args` array from here for the same reason.
const MCP_SERVER_ARGS: &[&str] = &["serve"];

/// GitHub Copilot agent.
pub struct CopilotIntegration;

impl AgentIntegration for CopilotIntegration {
    fn name(&self) -> &'static str {
        "GitHub Copilot"
    }

    fn id(&self) -> &'static str {
        "copilot"
    }

    /// Copilot's registration surfaces are all user-scope: the CLI-owned
    /// `~/.copilot/mcp-config.json` (written by `copilot mcp add`) and the
    /// VS Code user `settings.json`. There is no project-local surface the
    /// host reads, so offering a local install would mean hand-writing files
    /// the adopted CLI lifecycle exists to eliminate — same ruling as Gemini.
    fn supports_local_install(&self) -> bool {
        false
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mGitHub Copilot integration\x1b[0m");
        doctor_check_vscode_settings(dc, &super::vscode_data_dir(&ctx.home), "VS Code");
        doctor_check_vscode_settings(
            dc,
            &super::vscode_insiders_data_dir(&ctx.home),
            "VS Code Insiders",
        );
        doctor_check_cli_settings(dc, &ctx.home);
    }

    fn is_detected(&self, home: &Path) -> bool {
        super::vscode_data_dir(home).join("User").is_dir()
            || super::vscode_insiders_data_dir(home).join("User").is_dir()
            || super::copilot_cli_dir(home).is_dir()
    }

    fn primary_config_path(&self, home: &Path) -> Option<PathBuf> {
        Some(vscode_settings_path(home))
    }

    /// Mutable registration paths for the exact selected components.
    ///
    /// `ContextMcp` is the CLI-driven half of this integration: the only file
    /// it mutates is Copilot's own `~/.copilot/mcp-config.json`, and the writer
    /// is `copilot mcp`, not TraceDecay. Naming that file (and its staged
    /// backup) here is what gives the component-set transaction rollback
    /// authority over the host command's effect; without it the observation
    /// recorded in [`run_copilot_mcp_step`] would have nothing to restore.
    /// Any other component set keeps the default inventory, which is the
    /// TraceDecay-written VS Code settings file.
    fn host_component_registration_paths(
        &self,
        components: &[super::host_bundle_v2::HostBundleComponentV1],
        home: &Path,
    ) -> Vec<PathBuf> {
        if components == [super::host_bundle_v2::HostBundleComponentV1::ContextMcp] {
            let path = copilot_cli_mcp_config_path(home);
            vec![path.clone(), config_backup_path(&path)]
        } else {
            self.host_registration_paths(home)
        }
    }

    /// Register the tracedecay MCP server through Copilot's own registry.
    ///
    /// Only the `ContextMcp` component is CLI-driven. The VS Code
    /// `settings.json` half is deliberately absent here: no host command
    /// writes it (see the module documentation), so there is nothing to drive.
    fn activate_deployed_host_component_registration(
        &self,
        components: &[super::host_bundle_v2::HostBundleComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        if components.contains(&super::host_bundle_v2::HostBundleComponentV1::ContextMcp) {
            let copilot_cli = require_copilot_cli()?;
            copilot_mcp_add_with(&copilot_cli, &ctx.home, &ctx.tracedecay_bin)?;
        }
        Ok(())
    }

    /// Mirror of [`Self::activate_deployed_host_component_registration`]:
    /// removal goes back through the same registry that performed the add, so
    /// deactivation reverses exactly what activation created.
    fn deactivate_deployed_host_component_registration(
        &self,
        components: &[super::host_bundle_v2::HostBundleComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        if components.contains(&super::host_bundle_v2::HostBundleComponentV1::ContextMcp) {
            let copilot_cli = require_copilot_cli()?;
            copilot_mcp_remove_with(&copilot_cli, &ctx.home)?;
        }
        Ok(())
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        let stable_settings_path = vscode_settings_path(home);
        let insiders_settings_path = vscode_insiders_settings_path(home);
        let cli_settings_path = copilot_cli_mcp_config_path(home);

        let vscode_has_tracedecay = if stable_settings_path.exists() {
            let json = load_jsonc_file(&stable_settings_path);
            let servers = json.get("mcp").and_then(|v| v.get("servers"));
            servers.and_then(|v| v.get("tracedecay")).is_some()
        } else {
            false
        };

        let insiders_has_tracedecay = if insiders_settings_path.exists() {
            let json = load_jsonc_file(&insiders_settings_path);
            let servers = json.get("mcp").and_then(|v| v.get("servers"));
            servers.and_then(|v| v.get("tracedecay")).is_some()
        } else {
            false
        };

        let cli_has_tracedecay = if cli_settings_path.exists() {
            let json = load_json_file(&cli_settings_path);
            let servers = json.get("mcpServers");
            servers.and_then(|v| v.get("tracedecay")).is_some()
        } else {
            false
        };

        vscode_has_tracedecay || insiders_has_tracedecay || cli_has_tracedecay
    }

    fn export_managed_skills(
        &self,
        home: &Path,
        profile_root: &Path,
    ) -> Result<Vec<crate::automation::skill_targets::SkillInstallSummary>> {
        if !self.has_tracedecay(home) {
            return Ok(Vec::new());
        }
        let prompt_paths = [
            super::vscode_data_dir(home).join("User/prompts/copilot-instructions.md"),
            super::vscode_insiders_data_dir(home).join("User/prompts/copilot-instructions.md"),
            super::copilot_cli_dir(home).join("copilot-instructions.md"),
        ];
        prompt_paths
            .iter()
            .filter(|path| path.exists())
            .map(|path| {
                crate::automation::skill_targets::install_managed_skills(
                    profile_root,
                    crate::automation::skill_targets::SkillInstallTarget::Agents,
                    path,
                )
            })
            .collect()
    }

    fn export_managed_skills_local(
        &self,
        project_root: &Path,
        profile_root: &Path,
    ) -> Result<Vec<crate::automation::skill_targets::SkillInstallSummary>> {
        let instructions = project_root.join(".github/copilot-instructions.md");
        if !workspace_mcp_has_tracedecay(project_root) || !instructions.exists() {
            return Ok(Vec::new());
        }
        Ok(vec![
            crate::automation::skill_targets::install_managed_skills(
                profile_root,
                crate::automation::skill_targets::SkillInstallTarget::Agents,
                &instructions,
            )?,
        ])
    }
}

fn workspace_mcp_has_tracedecay(project_root: &Path) -> bool {
    let settings_path = project_root.join(".vscode/mcp.json");
    if !settings_path.exists() {
        return false;
    }
    let json = load_jsonc_file(&settings_path);
    json.get("servers")
        .and_then(|servers| servers.get("tracedecay"))
        .is_some()
}

// ---------------------------------------------------------------------------
// Registration paths
// ---------------------------------------------------------------------------

/// VS Code user settings — the TraceDecay-written half. No host CLI writes
/// this file; see the module documentation.
fn vscode_settings_path(home: &Path) -> PathBuf {
    super::vscode_data_dir(home).join("User/settings.json")
}

/// VS Code Insiders user settings, same ownership as [`vscode_settings_path`].
fn vscode_insiders_settings_path(home: &Path) -> PathBuf {
    super::vscode_insiders_data_dir(home).join("User/settings.json")
}

/// Copilot CLI's own MCP registry document.
///
/// Derived from the admitted profile home rather than any ambient Copilot
/// environment variable: `host_cli::run_host_cli` clears the environment and
/// admits `home` as the child's `HOME`, so the host command resolves its
/// profile from exactly this directory. Reading it from anywhere else would
/// let an isolated lifecycle inspect a different profile than the one the host
/// command just wrote.
fn copilot_cli_mcp_config_path(home: &Path) -> PathBuf {
    super::copilot_cli_dir(home).join("mcp-config.json")
}

// ---------------------------------------------------------------------------
// Host-CLI-driven MCP registry lifecycle
// ---------------------------------------------------------------------------

/// Resolve Copilot's own CLI, or fail with the typed requirement.
///
/// Copilot owns `~/.copilot/mcp-config.json` through `copilot mcp`. Its CLI is
/// a hard requirement for that half of the lifecycle, not a preference with a
/// config-editing fallback: emulating host-owned registry writes is precisely
/// what the host-capability doctrine forbids.
fn require_copilot_cli() -> Result<PathBuf> {
    super::host_cli::require_host_cli(COPILOT_CLI, COPILOT_CLI_LIFECYCLE)
}

/// Drive Copilot's own registry to add the tracedecay MCP server.
///
/// Copilot's non-interactive form is
/// `copilot mcp add <name> [-e KEY=VALUE] -- <command> <args…>`: everything
/// after the `--` separator is the server's launch command line, so the
/// command and [`MCP_SERVER_ARGS`] are passed as plain trailing words rather
/// than as repeated flags. No `-e` pairs are passed; the tracedecay server
/// needs no registry-supplied environment.
///
/// Split from the trait method so tests can supply a fake CLI and an isolated
/// `HOME` without mutating the process environment.
fn copilot_mcp_add_with(copilot_cli: &Path, home: &Path, tracedecay_bin: &str) -> Result<()> {
    let mut args = vec!["mcp", "add", COPILOT_MCP_SERVER_NAME, "--", tracedecay_bin];
    args.extend(MCP_SERVER_ARGS.iter().copied());
    run_copilot_mcp_step(copilot_cli, &args, home)
}

/// Drive Copilot's own registry to drop the tracedecay MCP server.
///
/// ASSUMPTION: the removal verb is spelled `copilot mcp remove <name>`, the
/// counterpart of the surveyed `copilot mcp add <name>` in the same `copilot
/// mcp` command family. TraceDecay has no captured transcript of the removal
/// command, so this spelling is unverified. It is safe to be wrong here in
/// exactly one direction: an unknown subcommand exits non-zero and
/// [`run_copilot_mcp_step`] surfaces Copilot's own diagnosis instead of
/// falling back to editing the host-owned registry. Should a capture show a
/// different verb, change this one call site.
fn copilot_mcp_remove_with(copilot_cli: &Path, home: &Path) -> Result<()> {
    run_copilot_mcp_step(
        copilot_cli,
        &["mcp", "remove", COPILOT_MCP_SERVER_NAME],
        home,
    )
}

/// Run one `copilot mcp …` step, converting a failed invocation into the
/// host's own diagnosis.
///
/// The peer-server snapshot is a preservation guard: Copilot owns the registry
/// merge, but a buggy or changed host command must not be allowed to silently
/// discard an operator's other MCP servers. The exact post-command bytes are
/// also recorded through the active host transaction so its existing rollback
/// authority can restore the pre-command document when the command fails or a
/// later verification step rejects the effect.
fn run_copilot_mcp_step(copilot_cli: &Path, args: &[&str], home: &Path) -> Result<()> {
    let mcp_path = copilot_cli_mcp_config_path(home);
    let (_, peers_before) = read_mcp_config_observation(&mcp_path)?;
    let outcome = super::host_cli::run_host_cli(copilot_cli, args, home)?;
    // Snapshot once after the child exits. The bytes that pass the peer guard
    // are the bytes recorded for rollback; reading again after recording would
    // create a race in which a foreign writer could be absorbed into the
    // transaction's intended state and later overwritten during recovery.
    let (observed_bytes, peers_after) = read_mcp_config_observation(&mcp_path)?;
    if peers_before != peers_after {
        return Err(TraceDecayError::Config {
            message: format!(
                "`{}` changed peer MCP servers in {}; TraceDecay left the host state unaccepted",
                rendered_invocation(copilot_cli, args),
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

/// Render an invocation the way the operator would have typed it, for the
/// preservation refusal above.
fn rendered_invocation(copilot_cli: &Path, args: &[&str]) -> String {
    if args.is_empty() {
        copilot_cli.display().to_string()
    } else {
        format!("{} {}", copilot_cli.display(), args.join(" "))
    }
}

/// Exact registry-document bytes (absent when nothing is registered yet) and
/// the operator-owned peer MCP server entries read from that document.
type McpConfigObservation = (Option<Vec<u8>>, serde_json::Map<String, serde_json::Value>);

/// Return the operator-owned MCP servers in Copilot's registry document,
/// excluding TraceDecay's own entry. The host CLI remains the only writer;
/// this read-only snapshot lets the lifecycle reject a command that drops or
/// rewrites peers.
///
/// Read the registry document once, returning both its exact bytes (for the
/// rollback observation) and its peer servers (for the preservation guard).
/// An absent file is a legitimate state — nothing registered yet — and yields
/// no bytes and no peers.
fn read_mcp_config_observation(path: &Path) -> Result<McpConfigObservation> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((None, serde_json::Map::new()));
        }
        Err(error) => {
            return Err(TraceDecayError::Config {
                message: format!(
                    "failed to read {} before Copilot CLI: {error}",
                    path.display()
                ),
            });
        }
    };
    let config = serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|error| {
        TraceDecayError::Config {
            message: format!("failed to parse {} as JSON: {error}", path.display()),
        }
    })?;
    let Some(servers) = config.get("mcpServers") else {
        return Ok((Some(bytes), serde_json::Map::new()));
    };
    let Some(servers) = servers.as_object() else {
        return Err(TraceDecayError::Config {
            message: format!("{}.mcpServers must be a JSON object", path.display()),
        });
    };
    let peers = servers
        .iter()
        .filter(|(name, _)| name.as_str() != COPILOT_MCP_SERVER_NAME)
        .map(|(name, server)| (name.clone(), server.clone()))
        .collect();
    Ok((Some(bytes), peers))
}

// ---------------------------------------------------------------------------
// Healthcheck helpers
// ---------------------------------------------------------------------------

/// True when a registered server's `args` array carries every argument in
/// [`MCP_SERVER_ARGS`].
///
/// Binding the doctor's expectation to the same constant the CLI invocation
/// spells is what keeps the two from drifting: changing the launch arguments
/// cannot silently leave the readback checking a stale word.
fn server_args_are_current(server: &serde_json::Map<String, serde_json::Value>) -> bool {
    let Some(args) = server.get("args").and_then(|value| value.as_array()) else {
        return false;
    };
    MCP_SERVER_ARGS
        .iter()
        .all(|expected| args.iter().any(|arg| arg.as_str() == Some(*expected)))
}

/// Check VS Code (or VS Code Insiders) settings.json has tracedecay MCP server registered.
fn doctor_check_vscode_settings(dc: &mut DoctorCounters, vscode_dir: &Path, label: &str) {
    let settings_path = vscode_dir.join("User/settings.json");

    if !settings_path.exists() {
        dc.warn(&format!(
            "{} not found — run `tracedecay install --agent copilot` if you use GitHub Copilot in {label}",
            settings_path.display()
        ));
        return;
    }

    let settings = load_jsonc_file(&settings_path);
    let server = settings
        .get("mcp")
        .and_then(|v| v.get("servers"))
        .and_then(|v| v.get("tracedecay"));

    let Some(server) = server.and_then(|v| v.as_object()) else {
        dc.fail(&format!(
            "MCP server NOT registered in {} — run `tracedecay install --agent copilot`",
            settings_path.display()
        ));
        return;
    };
    dc.pass(&format!(
        "MCP server registered in {}",
        settings_path.display()
    ));

    // Check args carry the shared launch arguments (currently "serve").
    if server_args_are_current(server) {
        dc.pass("MCP server args include \"serve\"");
    } else {
        dc.fail("MCP server args missing \"serve\" — run `tracedecay install --agent copilot`");
    }
}

/// Check Copilot CLI mcp-config.json has tracedecay MCP server registered.
///
/// Read-only: this document is written by `copilot mcp`, never by TraceDecay.
fn doctor_check_cli_settings(dc: &mut DoctorCounters, home: &Path) {
    let settings_path = copilot_cli_mcp_config_path(home);

    if !settings_path.exists() {
        dc.warn(&format!(
            "{} not found — run `tracedecay install --agent copilot` if you use Copilot CLI",
            settings_path.display()
        ));
        return;
    }

    let settings = load_json_file(&settings_path);
    let server = settings.get("mcpServers").and_then(|v| v.get("tracedecay"));

    let Some(server) = server.and_then(|v| v.as_object()) else {
        dc.fail(&format!(
            "MCP server NOT registered in {} — run `tracedecay install --agent copilot`",
            settings_path.display()
        ));
        return;
    };
    dc.pass(&format!(
        "MCP server registered in {}",
        settings_path.display()
    ));

    if server_args_are_current(server) {
        dc.pass("MCP server args include \"serve\"");
    } else {
        dc.fail("MCP server args missing \"serve\" — run `tracedecay install --agent copilot`");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
