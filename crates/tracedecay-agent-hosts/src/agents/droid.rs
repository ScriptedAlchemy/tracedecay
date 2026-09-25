//! Factory Droid agent integration.
//!
//! Droid (the Factory agent CLI, https://factory.com/) owns
//! `~/.factory/mcp.json` through its own registry commands:
//!
//! - `droid mcp add tracedecay "<resolved tracedecay binary> serve" --type stdio`
//! - `droid mcp remove tracedecay`
//!
//! TraceDecay drives those commands and never merges `~/.factory/mcp.json`
//! itself: emulating host-owned registry writes is precisely what the
//! host-capability doctrine forbids. The one managed artifact is the
//! receipt-owned component descriptor under `.factory/tracedecay/`, exactly
//! Copilot's and Kiro's shape, for exactly their reason. The registration
//! readback (doctor and receipt verification) parses the host-owned document
//! at `/mcpServers/tracedecay` and checks the launch surface against the same
//! constants the CLI invocation spells.
//!
//! Droid also documents a hooks surface (`~/.factory/hooks.json`), but no
//! checked-in native Droid event fixture proves that route yet, so the Hooks
//! capability stays evidence-gated and the component set is Context MCP only.

use std::path::{Path, PathBuf};

use serde_json::Value;
use tracedecay_domain::errors::{Result, TraceDecayError};

use super::host_bundle::{HostBundleRegistrationStateV1, HostComponentV1};
use super::{AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext, load_json_file};

/// Name of Factory Droid's own CLI, which owns `~/.factory/mcp.json`.
const DROID_CLI: &str = "droid";

/// What the binary is required *for*, used in the typed absence error so the
/// operator learns both what is missing and which lifecycle needed it.
const DROID_CLI_LIFECYCLE: &str = "Factory Droid MCP registry lifecycle";

/// Name Droid's registry selects the server by (`droid mcp add <name>`,
/// `droid mcp remove <name>`) and the key it lands under in `mcpServers`.
const DROID_MCP_SERVER_NAME: &str = "tracedecay";

/// Arguments the tracedecay MCP server is launched with, shared by the
/// CLI-driven registration and the doctor readback so the two spellings of
/// the same server cannot drift apart.
const MCP_SERVER_ARGS: &[&str] = &["serve"];

/// Droid's stdio transport spelling, passed explicitly so the stored
/// registration pins the transport instead of relying on the CLI default.
const DROID_TRANSPORT: &str = "stdio";

pub struct DroidIntegration;

fn droid_config_dir(home: &Path) -> PathBuf {
    home.join(".factory")
}

fn droid_mcp_config_path(home: &Path) -> PathBuf {
    droid_config_dir(home).join("mcp.json")
}

/// Readback state for the one host-owned registration this integration
/// drives: the `mcpServers.tracedecay` entry inside `~/.factory/mcp.json`.
fn droid_context_mcp_registration_state(home: &Path) -> HostBundleRegistrationStateV1 {
    let config_path = droid_mcp_config_path(home);
    let Ok(config_bytes) = std::fs::read(&config_path) else {
        return HostBundleRegistrationStateV1::Missing;
    };
    let Ok(config) = serde_json::from_slice::<Value>(&config_bytes) else {
        return HostBundleRegistrationStateV1::Corrupt;
    };
    let Some(server) = config
        .pointer("/mcpServers/tracedecay")
        .and_then(Value::as_object)
    else {
        return HostBundleRegistrationStateV1::Missing;
    };
    let transport_is_stdio = server
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|transport| transport == DROID_TRANSPORT);
    let command_is_present = server
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(|command| !command.is_empty());
    if transport_is_stdio && command_is_present && server_args_are_current(server) {
        HostBundleRegistrationStateV1::Current
    } else {
        HostBundleRegistrationStateV1::Repairable
    }
}

impl AgentIntegration for DroidIntegration {
    fn name(&self) -> &'static str {
        "Factory Droid"
    }

    fn id(&self) -> &'static str {
        "droid"
    }

    /// Droid's registration surface is user-scope only: the CLI-owned
    /// `~/.factory/mcp.json`, written by `droid mcp add`. There is no
    /// project-local surface the host reads for MCP servers, so offering a
    /// local install would mean hand-writing files the adopted CLI lifecycle
    /// exists to eliminate, the same ruling as Gemini and Copilot.
    fn supports_local_install(&self) -> bool {
        false
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mFactory Droid integration\x1b[0m");
        let config_path = droid_mcp_config_path(&ctx.home);
        if !config_path.exists() {
            dc.warn(&format!(
                "{} not found, run `tracedecay install --agent droid` if you use Factory Droid",
                config_path.display()
            ));
            return;
        }
        let config = load_json_file(&config_path);
        let entry = &config["mcpServers"]["tracedecay"];
        if !entry.is_object() {
            dc.fail(&format!(
                "MCP server NOT registered in {}, run `tracedecay install --agent droid`",
                config_path.display()
            ));
            return;
        }
        let args_current = entry
            .get("args")
            .and_then(Value::as_array)
            .is_some_and(|args| {
                MCP_SERVER_ARGS
                    .iter()
                    .all(|expected| args.iter().any(|arg| arg.as_str() == Some(expected)))
            });
        if args_current {
            dc.pass(&format!(
                "MCP server registered in {}",
                config_path.display()
            ));
        } else {
            dc.fail(&format!(
                "MCP server registered in {} but args are stale, run `tracedecay install --agent droid`",
                config_path.display()
            ));
        }
    }

    fn host_component_registration(
        &self,
        component: HostComponentV1,
        ctx: &HealthcheckContext,
    ) -> HostBundleRegistrationStateV1 {
        if component != HostComponentV1::ContextMcp {
            return HostBundleRegistrationStateV1::Missing;
        }
        droid_context_mcp_registration_state(&ctx.home)
    }

    fn is_detected(&self, home: &Path) -> bool {
        droid_config_dir(home).is_dir()
    }

    fn primary_config_path(&self, home: &Path) -> Option<PathBuf> {
        Some(droid_mcp_config_path(home))
    }

    fn host_registration_paths(&self, home: &Path) -> Vec<PathBuf> {
        vec![droid_mcp_config_path(home)]
    }

    /// Register the tracedecay MCP server through Droid's own registry.
    ///
    /// The add form is `droid mcp add tracedecay "<command> serve" --type
    /// stdio`: the launch command is one argv word that the host CLI splits
    /// into `command` + `args`, matching its documented non-interactive form.
    fn activate_deployed_host_component_registration(
        &self,
        components: &[HostComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        if components.contains(&HostComponentV1::ContextMcp) {
            let droid_cli = require_droid_cli()?;
            droid_mcp_add_with(&droid_cli, &ctx.home, &ctx.tracedecay_bin)?;
        }
        Ok(())
    }

    /// Mirror of [`Self::activate_deployed_host_component_registration`]:
    /// removal goes back through the same registry that performed the add.
    fn deactivate_deployed_host_component_registration(
        &self,
        components: &[HostComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        if components.contains(&HostComponentV1::ContextMcp) {
            let droid_cli = require_droid_cli()?;
            droid_mcp_remove_with(&droid_cli, &ctx.home)?;
        }
        Ok(())
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        super::mcp_config_has_tracedecay(&droid_mcp_config_path(home), "mcpServers", load_json_file)
    }

    fn detected_host_surface(&self, home: &Path) -> Option<PathBuf> {
        droid_config_dir(home)
            .is_dir()
            .then(|| droid_config_dir(home))
    }
}

// ---------------------------------------------------------------------------
// Host-CLI-driven MCP registry lifecycle
// ---------------------------------------------------------------------------

/// Resolve Droid's own CLI, or fail with the typed requirement. Droid owns
/// `~/.factory/mcp.json` through `droid mcp`; its CLI is a hard requirement
/// for the lifecycle, not a preference with a config-editing fallback.
fn require_droid_cli() -> Result<PathBuf> {
    super::host_cli::require_host_cli(DROID_CLI, DROID_CLI_LIFECYCLE)
}

fn read_optional_config_bytes(path: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(TraceDecayError::Config {
            message: format!("failed to read {}: {error}", path.display()),
        }),
    }
}

/// Drive Droid's own registry to add the tracedecay MCP server.
///
/// Split from the trait method so tests can supply a fake CLI and an isolated
/// `HOME` without mutating the process environment. A prior TraceDecay
/// registration is removed first so a reinstall refreshes the launch surface
/// instead of failing on the duplicate name; on add failure the previous
/// document bytes are restored when the host left them unchanged.
#[hotpath::measure(label = "droid_mcp_install")]
fn droid_mcp_add_with(droid_cli: &Path, home: &Path, tracedecay_bin: &str) -> Result<()> {
    let config_path = droid_mcp_config_path(home);
    let previous_registration =
        if super::mcp_config_has_tracedecay(&config_path, "mcpServers", load_json_file) {
            let bytes = read_optional_config_bytes(&config_path)?.ok_or_else(|| {
                TraceDecayError::Config {
                    message: format!(
                        "{} disappeared while preparing the Droid MCP refresh",
                        config_path.display()
                    ),
                }
            })?;
            let metadata = super::capture_host_file_metadata(&config_path)?;
            Some((bytes, metadata))
        } else {
            None
        };
    if previous_registration.is_some() {
        droid_mcp_remove_with(droid_cli, home)?;
    }
    let launch_command = format!("{tracedecay_bin} serve");
    let args = vec![
        "mcp",
        "add",
        DROID_MCP_SERVER_NAME,
        launch_command.as_str(),
        "--type",
        DROID_TRANSPORT,
    ];
    let result = super::host_cli::run_mcp_registry_step(
        droid_cli,
        &args,
        home,
        &config_path,
        DROID_MCP_SERVER_NAME,
        "Droid CLI",
    );
    let Err(add_error) = result else {
        return Ok(());
    };
    let Some((previous_bytes, previous_metadata)) = previous_registration else {
        return Err(add_error);
    };
    let current_bytes = read_optional_config_bytes(&config_path)?;
    if let Err(restore_error) = super::text_file_transaction::restore_bytes_file_if_unchanged(
        &config_path,
        current_bytes.as_deref(),
        &previous_bytes,
        &previous_metadata,
    ) {
        return Err(TraceDecayError::Config {
            message: format!(
                "{add_error}; restoring the previous Droid MCP registration also failed: \
                 {restore_error}"
            ),
        });
    }
    Err(add_error)
}

/// Drive Droid's own registry to drop the tracedecay MCP server, the exact
/// counterpart of the documented `droid mcp add` in the same command family.
fn droid_mcp_remove_with(droid_cli: &Path, home: &Path) -> Result<()> {
    super::host_cli::run_mcp_registry_step(
        droid_cli,
        &["mcp", "remove", DROID_MCP_SERVER_NAME],
        home,
        &droid_mcp_config_path(home),
        DROID_MCP_SERVER_NAME,
        "Droid CLI",
    )
}

// ---------------------------------------------------------------------------
// Healthcheck helpers
// ---------------------------------------------------------------------------

/// True when a registered server's `args` array carries every argument in
/// [`MCP_SERVER_ARGS`]. Binding the doctor's expectation to the same constant
/// the CLI invocation spells keeps the two from drifting.
fn server_args_are_current(server: &serde_json::Map<String, Value>) -> bool {
    let Some(args) = server.get("args").and_then(Value::as_array) else {
        return false;
    };
    MCP_SERVER_ARGS
        .iter()
        .all(|expected| args.iter().any(|arg| arg.as_str() == Some(*expected)))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn write_config(home: &Path, config: Value) {
        std::fs::create_dir_all(droid_config_dir(home)).unwrap();
        std::fs::write(
            droid_mcp_config_path(home),
            format!("{}\n", serde_json::to_string(&config).unwrap()),
        )
        .unwrap();
    }

    #[test]
    fn registration_state_reads_the_host_owned_document() {
        let home = tempfile::tempdir().unwrap();
        let health = HealthcheckContext {
            home: home.path().to_path_buf(),
            project_path: home.path().to_path_buf(),
        };
        let integration = DroidIntegration;

        assert_eq!(
            integration.host_component_registration(HostComponentV1::ContextMcp, &health),
            HostBundleRegistrationStateV1::Missing
        );

        write_config(
            home.path(),
            serde_json::json!({
                "mcpServers": {
                    "tracedecay": {
                        "type": "stdio",
                        "command": "/usr/local/bin/tracedecay",
                        "args": ["serve"],
                        "disabled": false
                    },
                    "peer": { "type": "http", "url": "https://example.invalid/mcp" }
                }
            }),
        );
        assert_eq!(
            integration.host_component_registration(HostComponentV1::ContextMcp, &health),
            HostBundleRegistrationStateV1::Current
        );
        assert!(integration.has_tracedecay(home.path()));

        // A foreign entry at the deploy key that lost the launch surface is
        // repairable, never claimed current.
        write_config(
            home.path(),
            serde_json::json!({
                "mcpServers": {
                    "tracedecay": {
                        "type": "stdio",
                        "command": "/usr/local/bin/tracedecay",
                        "args": ["something-else"]
                    }
                }
            }),
        );
        assert_eq!(
            integration.host_component_registration(HostComponentV1::ContextMcp, &health),
            HostBundleRegistrationStateV1::Repairable
        );

        // Other components are absent for this host.
        assert_eq!(
            integration.host_component_registration(HostComponentV1::Core, &health),
            HostBundleRegistrationStateV1::Missing
        );
    }

    #[test]
    fn detection_and_config_paths_stay_inside_the_factory_directory() {
        let home = tempfile::tempdir().unwrap();
        let integration = DroidIntegration;
        assert!(!integration.is_detected(home.path()));
        assert!(!integration.has_tracedecay(home.path()));

        std::fs::create_dir_all(droid_config_dir(home.path())).unwrap();
        assert!(integration.is_detected(home.path()));
        assert_eq!(
            integration.primary_config_path(home.path()),
            Some(droid_mcp_config_path(home.path()))
        );
        assert_eq!(
            integration.host_registration_paths(home.path()),
            vec![droid_mcp_config_path(home.path())]
        );
        assert!(droid_mcp_config_path(home.path()).starts_with(home.path()));
    }
}
