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
//! Droid's hooks surface (`~/.factory/hooks.json`) is a host-owned
//! configuration file, not a registry with its own CLI, so the `Core`
//! component manages a merge into it: `SessionStart` and `Stop` entries
//! calling `tracedecay hook-droid-event` under the Droid native identity,
//! whose payload shape is proven by the checked-in captured fixture
//! (`crates/tracedecay-hooks/fixtures/host_events/droid.json`). Operator hook
//! entries are preserved byte-for-byte on install, refresh, and uninstall.

use std::path::{Path, PathBuf};
use tracedecay_runtime_core::config::ProfileRoot;

use serde_json::{Value, json};
use tracedecay_domain::errors::{Result, TraceDecayError};

use super::host_bundle::{HostBundleRegistrationStateV1, HostComponentV1};
use super::{
    AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext, JsonConfigDialect,
    JsonConfigMutation, load_json_file, update_json_config_transactionally,
};

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

/// The lifecycle events TraceDecay deploys into `~/.factory/hooks.json`,
/// matching the events proven by the checked-in captured fixture.
const DROID_HOOK_EVENTS: [&str; 2] = ["SessionStart", "Stop"];

/// The subcommand every managed Droid hook entry calls; also the ownership
/// marker used to distinguish TraceDecay entries from operator entries.
const DROID_HOOK_MARKER: &str = "hook-droid-event";

/// Timeout (seconds) for the managed hook commands, bounded like the other
/// host hook tables.
const DROID_HOOK_TIMEOUT_SECS: u64 = 30;

pub struct DroidIntegration;

fn droid_config_dir(home: &Path) -> PathBuf {
    home.join(".factory")
}

fn droid_mcp_config_path(home: &Path) -> PathBuf {
    droid_config_dir(home).join("mcp.json")
}

fn droid_hooks_path(home: &Path) -> PathBuf {
    droid_config_dir(home).join("hooks.json")
}

/// The managed hook entry TraceDecay appends under each lifecycle event.
fn tracedecay_hook_entry(tracedecay_bin: &str) -> Value {
    json!({
        "type": "command",
        "command": super::hook_command(tracedecay_bin, DROID_HOOK_MARKER),
        "timeout": DROID_HOOK_TIMEOUT_SECS,
    })
}

/// True when a hook group carries the TraceDecay marker command.
fn hook_group_is_tracedecay(group: &Value) -> bool {
    group
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|hooks| {
            hooks.iter().any(|hook| {
                hook.get("command")
                    .and_then(Value::as_str)
                    .is_some_and(|command| command.contains(DROID_HOOK_MARKER))
            })
        })
}

/// Merge the managed SessionStart / Stop entries into the host-owned hooks
/// document, replacing any earlier TraceDecay entries and preserving every
/// operator entry byte-for-byte. Returns true when the document changed.
fn install_droid_hooks(hooks_path: &Path, tracedecay_bin: &str) -> Result<bool> {
    update_json_config_transactionally(hooks_path, JsonConfigDialect::Json, |mut config| {
        let object = config
            .as_object_mut()
            .ok_or_else(|| TraceDecayError::Config {
                message: format!("{} must contain a JSON object", hooks_path.display()),
            })?;
        let mut changed = false;
        for event in DROID_HOOK_EVENTS {
            let groups = object
                .entry(event)
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .ok_or_else(|| TraceDecayError::Config {
                    message: format!(
                        "{} key in {} must contain a JSON array",
                        event,
                        hooks_path.display()
                    ),
                })?;
            let before = groups.clone();
            groups.retain(|group| !hook_group_is_tracedecay(group));
            groups.push(json!({
                "matcher": "*",
                "hooks": [tracedecay_hook_entry(tracedecay_bin)],
            }));
            changed |= *groups != before;
        }
        if changed {
            Ok((true, JsonConfigMutation::Write(config)))
        } else {
            Ok((false, JsonConfigMutation::Unchanged))
        }
    })
}

/// Remove every TraceDecay hook group from the host-owned hooks document and
/// drop event keys that become empty. Returns true when the document changed.
fn remove_droid_hooks(hooks_path: &Path) -> Result<bool> {
    update_json_config_transactionally(hooks_path, JsonConfigDialect::Json, |mut config| {
        let Some(object) = config.as_object_mut() else {
            return Ok((false, JsonConfigMutation::Unchanged));
        };
        let mut changed = false;
        for event in DROID_HOOK_EVENTS {
            let Some(groups) = object.get_mut(event).and_then(Value::as_array_mut) else {
                continue;
            };
            let before = groups.len();
            groups.retain(|group| !hook_group_is_tracedecay(group));
            changed |= groups.len() != before;
            if groups.is_empty() {
                object.remove(event);
            }
        }
        if changed {
            Ok((true, JsonConfigMutation::Write(config)))
        } else {
            Ok((false, JsonConfigMutation::Unchanged))
        }
    })
}

/// Readback state for the managed hook merge: every deployed event must carry
/// a TraceDecay hook group; a partial set is repairable, none is missing.
fn droid_hooks_registration_state(home: &Path) -> HostBundleRegistrationStateV1 {
    let hooks_path = droid_hooks_path(home);
    let Ok(bytes) = std::fs::read(&hooks_path) else {
        return HostBundleRegistrationStateV1::Missing;
    };
    let Ok(config) = serde_json::from_slice::<Value>(&bytes) else {
        return HostBundleRegistrationStateV1::Corrupt;
    };
    let events = DROID_HOOK_EVENTS
        .iter()
        .map(|event| {
            config
                .get(event)
                .and_then(Value::as_array)
                .is_some_and(|groups| groups.iter().any(hook_group_is_tracedecay))
        })
        .collect::<Vec<_>>();
    if events.iter().all(|present| *present) {
        HostBundleRegistrationStateV1::Current
    } else if events.iter().any(|present| *present) {
        HostBundleRegistrationStateV1::Repairable
    } else {
        HostBundleRegistrationStateV1::Missing
    }
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
        let hooks_path = droid_hooks_path(&ctx.home);
        match droid_hooks_registration_state(&ctx.home) {
            HostBundleRegistrationStateV1::Current => dc.pass(&format!(
                "TraceDecay hooks merged into {}",
                hooks_path.display()
            )),
            HostBundleRegistrationStateV1::Repairable => dc.fail(&format!(
                "TraceDecay hooks in {} are incomplete, run `tracedecay install --agent droid`",
                hooks_path.display()
            )),
            HostBundleRegistrationStateV1::Missing => dc.warn(&format!(
                "{} has no TraceDecay hooks, run `tracedecay install --agent droid`",
                hooks_path.display()
            )),
            HostBundleRegistrationStateV1::Corrupt => dc.fail(&format!(
                "{} could not be parsed as JSON",
                hooks_path.display()
            )),
        }
    }

    fn host_component_registration(
        &self,
        component: HostComponentV1,
        ctx: &HealthcheckContext,
    ) -> HostBundleRegistrationStateV1 {
        match component {
            HostComponentV1::ContextMcp => droid_context_mcp_registration_state(&ctx.home),
            HostComponentV1::Core => droid_hooks_registration_state(&ctx.home),
            HostComponentV1::Agent | HostComponentV1::OperatorMcp => {
                HostBundleRegistrationStateV1::Missing
            }
        }
    }

    fn is_detected(&self, home: &Path) -> bool {
        droid_config_dir(home).is_dir()
    }

    fn primary_config_path(&self, home: &Path, _profile: &ProfileRoot) -> Option<PathBuf> {
        Some(droid_mcp_config_path(home))
    }

    fn host_registration_paths(&self, home: &Path, _profile: &ProfileRoot) -> Vec<PathBuf> {
        vec![droid_mcp_config_path(home), droid_hooks_path(home)]
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
        if components.contains(&HostComponentV1::Core) {
            install_droid_hooks(&droid_hooks_path(&ctx.home), &ctx.tracedecay_bin)?;
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
        if components.contains(&HostComponentV1::Core) {
            remove_droid_hooks(&droid_hooks_path(&ctx.home))?;
        }
        Ok(())
    }

    fn has_tracedecay(&self, home: &Path, _profile: &ProfileRoot) -> bool {
        super::mcp_config_has_tracedecay(&droid_mcp_config_path(home), "mcpServers", load_json_file)
            || droid_hooks_registration_state(home) != HostBundleRegistrationStateV1::Missing
    }

    fn detected_host_surface(&self, home: &Path, _profile: &ProfileRoot) -> Option<PathBuf> {
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

    fn write_hooks(home: &Path, hooks: Value) {
        std::fs::create_dir_all(droid_config_dir(home)).unwrap();
        std::fs::write(
            droid_hooks_path(home),
            format!("{}\n", serde_json::to_string(&hooks).unwrap()),
        )
        .unwrap();
    }

    #[test]
    fn hook_merge_preserves_operator_entries_and_installs_both_events() {
        let home = tempfile::tempdir().unwrap();
        write_hooks(
            home.path(),
            serde_json::json!({
                "SessionStart": [
                    {
                        "matcher": "*",
                        "hooks": [
                            { "type": "command", "command": "/usr/local/bin/operator-hook.sh", "timeout": 10 }
                        ]
                    }
                ]
            }),
        );

        let changed =
            install_droid_hooks(&droid_hooks_path(home.path()), "/usr/local/bin/tracedecay")
                .unwrap();
        assert!(changed);

        let merged: Value =
            serde_json::from_slice(&std::fs::read(droid_hooks_path(home.path())).unwrap()).unwrap();
        for event in DROID_HOOK_EVENTS {
            let groups = merged[event].as_array().unwrap();
            assert!(
                groups.iter().any(hook_group_is_tracedecay),
                "{event} must carry a TraceDecay group"
            );
        }
        let session_start = merged["SessionStart"].as_array().unwrap();
        assert_eq!(session_start.len(), 2);
        assert_eq!(
            session_start[0]["hooks"][0]["command"],
            "/usr/local/bin/operator-hook.sh"
        );
        assert_eq!(
            session_start[1]["hooks"][0]["command"],
            crate::agents::hook_command("/usr/local/bin/tracedecay", DROID_HOOK_MARKER)
        );
        assert_eq!(session_start[1]["hooks"][0]["timeout"], 30);
        assert_eq!(
            droid_hooks_registration_state(home.path()),
            HostBundleRegistrationStateV1::Current
        );
        assert_eq!(
            DroidIntegration.host_component_registration(
                HostComponentV1::Core,
                &HealthcheckContext {
                    profile: tracedecay_runtime_core::config::ProfileRoot::under_home(home.path()),
                    home: home.path().to_path_buf(),
                    project_path: home.path().to_path_buf(),
                }
            ),
            HostBundleRegistrationStateV1::Current
        );

        // A refresh with the same binary is a no-op, never a twin group.
        let changed =
            install_droid_hooks(&droid_hooks_path(home.path()), "/usr/local/bin/tracedecay")
                .unwrap();
        assert!(!changed);
        let refreshed: Value =
            serde_json::from_slice(&std::fs::read(droid_hooks_path(home.path())).unwrap()).unwrap();
        assert_eq!(refreshed["SessionStart"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn hook_removal_leaves_operator_entries_and_drops_empty_events() {
        let home = tempfile::tempdir().unwrap();
        write_hooks(
            home.path(),
            serde_json::json!({
                "SessionStart": [
                    {
                        "matcher": "*",
                        "hooks": [
                            { "type": "command", "command": "/usr/local/bin/operator-hook.sh", "timeout": 10 }
                        ]
                    },
                    {
                        "matcher": "*",
                        "hooks": [
                            { "type": "command", "command": "/usr/local/bin/tracedecay hook-droid-event", "timeout": 30 }
                        ]
                    }
                ],
                "Stop": [
                    {
                        "matcher": "*",
                        "hooks": [
                            { "type": "command", "command": "/usr/local/bin/tracedecay hook-droid-event", "timeout": 30 }
                        ]
                    }
                ]
            }),
        );

        let changed = remove_droid_hooks(&droid_hooks_path(home.path())).unwrap();
        assert!(changed);
        let remaining: Value =
            serde_json::from_slice(&std::fs::read(droid_hooks_path(home.path())).unwrap()).unwrap();
        assert_eq!(remaining["SessionStart"].as_array().unwrap().len(), 1);
        assert_eq!(
            remaining["SessionStart"][0]["hooks"][0]["command"],
            "/usr/local/bin/operator-hook.sh"
        );
        assert!(
            remaining.get("Stop").is_none(),
            "emptied events are dropped"
        );
        assert_eq!(
            droid_hooks_registration_state(home.path()),
            HostBundleRegistrationStateV1::Missing
        );

        // A missing document is a no-op removal, never a failure.
        std::fs::remove_file(droid_hooks_path(home.path())).unwrap();
        let changed = remove_droid_hooks(&droid_hooks_path(home.path())).unwrap();
        assert!(!changed);
    }

    #[test]
    fn registration_state_reads_the_host_owned_document() {
        let home = tempfile::tempdir().unwrap();
        let health = HealthcheckContext {
            profile: tracedecay_runtime_core::config::ProfileRoot::under_home(home.path()),
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
        assert!(integration.has_tracedecay(
            home.path(),
            &tracedecay_runtime_core::config::ProfileRoot::under_home(home.path())
        ));

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
        assert!(!integration.has_tracedecay(
            home.path(),
            &tracedecay_runtime_core::config::ProfileRoot::under_home(home.path())
        ));

        std::fs::create_dir_all(droid_config_dir(home.path())).unwrap();
        assert!(integration.is_detected(home.path()));
        assert_eq!(
            integration.primary_config_path(
                home.path(),
                &tracedecay_runtime_core::config::ProfileRoot::under_home(home.path())
            ),
            Some(droid_mcp_config_path(home.path()))
        );
        assert_eq!(
            integration.host_registration_paths(
                home.path(),
                &tracedecay_runtime_core::config::ProfileRoot::under_home(home.path())
            ),
            vec![
                droid_mcp_config_path(home.path()),
                droid_hooks_path(home.path())
            ]
        );
        assert!(droid_mcp_config_path(home.path()).starts_with(home.path()));
    }

    const OPERATOR_HOOKS: &str = r#"{
    "SessionStart": [
        {
            "matcher": "startup",
            "hooks": [
                { "type": "command", "command": "/usr/local/bin/operator-hook.sh", "timeout": 10 }
            ]
        }
    ]
}
"#;

    #[test]
    fn hook_install_and_uninstall_edit_the_operator_document_in_place() {
        let home = tempfile::tempdir().unwrap();
        let hooks_path = droid_hooks_path(home.path());
        std::fs::create_dir_all(droid_config_dir(home.path())).unwrap();
        std::fs::write(&hooks_path, OPERATOR_HOOKS).unwrap();

        assert!(install_droid_hooks(&hooks_path, "/usr/local/bin/tracedecay").unwrap());
        let installed = std::fs::read_to_string(&hooks_path).unwrap();
        assert_eq!(
            installed,
            r#"{
    "SessionStart": [
        {
            "matcher": "startup",
            "hooks": [
                { "type": "command", "command": "/usr/local/bin/operator-hook.sh", "timeout": 10 }
            ]
        },
        {
            "hooks": [
                {
                    "command": "'/usr/local/bin/tracedecay' hook-droid-event",
                    "timeout": 30,
                    "type": "command"
                }
            ],
            "matcher": "*"
        }
    ],
    "Stop": [
        {
            "hooks": [
                {
                    "command": "'/usr/local/bin/tracedecay' hook-droid-event",
                    "timeout": 30,
                    "type": "command"
                }
            ],
            "matcher": "*"
        }
    ]
}
"#
        );

        assert!(remove_droid_hooks(&hooks_path).unwrap());
        assert_eq!(
            std::fs::read_to_string(&hooks_path).unwrap(),
            OPERATOR_HOOKS
        );
    }

    /// Install a fake `droid` that appends each invocation's argv to `log` and
    /// then performs `body`. The child runs with no `PATH`, so bodies spell
    /// absolute tool paths.
    #[cfg(unix)]
    fn fake_droid_cli(bin: &Path, log: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{log}'\n{body}\n",
            log = log.display(),
        );
        std::fs::write(bin, script).unwrap();
        let mut permissions = std::fs::metadata(bin).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(bin, permissions).unwrap();
    }

    #[cfg(unix)]
    const PEER_ONLY: &str =
        r#"{"mcpServers":{"peer":{"type":"http","url":"https://example.invalid/mcp"}}}"#;

    /// Emulates Droid's registry: `mcp add` splits the one-word launch command
    /// into `command` + `args` beside the operator's peer, `mcp remove` drops
    /// only the tracedecay entry.
    #[cfg(unix)]
    const FAKE_REGISTRY_BODY: &str = r#"case "$1 $2" in
  "mcp add")
    [ "$5 $6" = "--type stdio" ] || { echo 'missing --type stdio' >&2; exit 64; }
    command=$(printf '%s' "$4" | /usr/bin/sed 's/ serve$//')
    /bin/mkdir -p "$HOME/.factory"
    printf '{"mcpServers":{"peer":{"type":"http","url":"https://example.invalid/mcp"},"tracedecay":{"type":"stdio","command":"%s","args":["serve"]}}}\n' "$command" > "$HOME/.factory/mcp.json"
    ;;
  "mcp remove")
    printf '%s\n' '{"mcpServers":{"peer":{"type":"http","url":"https://example.invalid/mcp"}}}' > "$HOME/.factory/mcp.json"
    ;;
esac
exit 0"#;

    #[cfg(unix)]
    fn invocations(log: &Path) -> Vec<String> {
        std::fs::read_to_string(log)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    #[cfg(unix)]
    #[test]
    fn registry_add_and_remove_go_through_the_droid_cli_and_keep_the_peer() {
        let home = tempfile::tempdir().unwrap();
        let bin_dir = tempfile::tempdir().unwrap();
        let log = bin_dir.path().join("invocations.log");
        let droid_cli = bin_dir.path().join("droid");
        fake_droid_cli(&droid_cli, &log, FAKE_REGISTRY_BODY);
        let mcp_path = droid_mcp_config_path(home.path());
        std::fs::create_dir_all(droid_config_dir(home.path())).unwrap();
        std::fs::write(&mcp_path, format!("{PEER_ONLY}\n")).unwrap();

        droid_mcp_add_with(&droid_cli, home.path(), "/bin/tracedecay").unwrap();
        assert_eq!(
            std::fs::read_to_string(&mcp_path).unwrap(),
            "{\"mcpServers\":{\"peer\":{\"type\":\"http\",\"url\":\"https://example.invalid/mcp\"},\"tracedecay\":{\"type\":\"stdio\",\"command\":\"/bin/tracedecay\",\"args\":[\"serve\"]}}}\n"
        );
        assert_eq!(
            droid_context_mcp_registration_state(home.path()),
            HostBundleRegistrationStateV1::Current
        );

        droid_mcp_remove_with(&droid_cli, home.path()).unwrap();
        assert_eq!(
            invocations(&log),
            [
                "mcp add tracedecay /bin/tracedecay serve --type stdio",
                "mcp remove tracedecay",
            ]
        );
        assert_eq!(
            std::fs::read_to_string(&mcp_path).unwrap(),
            format!("{PEER_ONLY}\n")
        );
        assert_eq!(
            droid_context_mcp_registration_state(home.path()),
            HostBundleRegistrationStateV1::Missing
        );
    }

    #[cfg(unix)]
    #[test]
    fn refresh_removes_the_old_registration_before_adding_the_new_binary() {
        let home = tempfile::tempdir().unwrap();
        let bin_dir = tempfile::tempdir().unwrap();
        let log = bin_dir.path().join("invocations.log");
        let droid_cli = bin_dir.path().join("droid");
        fake_droid_cli(&droid_cli, &log, FAKE_REGISTRY_BODY);
        let mcp_path = droid_mcp_config_path(home.path());
        std::fs::create_dir_all(droid_config_dir(home.path())).unwrap();
        std::fs::write(
            &mcp_path,
            r#"{"mcpServers":{"peer":{"type":"http","url":"https://example.invalid/mcp"},"tracedecay":{"type":"stdio","command":"/old/tracedecay","args":["serve"]}}}"#,
        )
        .unwrap();

        droid_mcp_add_with(&droid_cli, home.path(), "/new/tracedecay").unwrap();

        assert_eq!(
            invocations(&log),
            [
                "mcp remove tracedecay",
                "mcp add tracedecay /new/tracedecay serve --type stdio",
            ]
        );
        let config: Value = serde_json::from_slice(&std::fs::read(&mcp_path).unwrap()).unwrap();
        assert_eq!(
            config["mcpServers"]["tracedecay"]["command"],
            "/new/tracedecay"
        );
    }

    #[cfg(unix)]
    #[test]
    fn failed_refresh_restores_the_exact_previous_registration() {
        let home = tempfile::tempdir().unwrap();
        let bin_dir = tempfile::tempdir().unwrap();
        let log = bin_dir.path().join("invocations.log");
        let droid_cli = bin_dir.path().join("droid");
        fake_droid_cli(
            &droid_cli,
            &log,
            r#"case "$1 $2" in
  "mcp remove")
    printf '%s\n' '{"mcpServers":{"peer":{"type":"http","url":"https://example.invalid/mcp"}}}' > "$HOME/.factory/mcp.json"
    ;;
  "mcp add")
    echo 'replacement registration rejected' >&2
    exit 17
    ;;
esac
exit 0"#,
        );
        let mcp_path = droid_mcp_config_path(home.path());
        std::fs::create_dir_all(droid_config_dir(home.path())).unwrap();
        let original = r#"{"mcpServers":{"peer":{"type":"http","url":"https://example.invalid/mcp"},"tracedecay":{"type":"stdio","command":"/old/tracedecay","args":["serve"]}}}"#;
        std::fs::write(&mcp_path, original).unwrap();

        let error = droid_mcp_add_with(&droid_cli, home.path(), "/new/tracedecay").unwrap_err();

        assert!(
            error
                .to_string()
                .contains("replacement registration rejected"),
            "{error}"
        );
        assert_eq!(std::fs::read_to_string(&mcp_path).unwrap(), original);
    }
}
