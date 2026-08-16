// Rust guideline compliant 2026-08-08
//! Codex's own non-interactive MCP registry, driven for the MCP-only install.
//!
//! # What Codex owns, and what TraceDecay is allowed to do
//!
//! Codex's *plugin* lifecycle is driven separately by [`super::plugin_registry`]
//! (`codex plugin add` / `remove`, probed non-interactive on Codex CLI 0.147.0).
//! This module never drives a plugin install, and it never writes
//! `~/.codex/config.toml`, which is where Codex records its own
//! `tracedecay@<marketplace>` activation keys and its `[hooks.state]` trust
//! hashes. The region guard below refuses any MCP command that disturbs those
//! plugin-owned regions.
//!
//! Codex's **MCP registry**, by contrast, *is* documented as non-interactive:
//!
//! ```text
//! codex mcp add <name> [--env KEY=VALUE ...] -- <command> [args...]
//! codex mcp remove <name>
//! codex mcp list | codex mcp get <name>
//! ```
//!
//! That is a host capability TraceDecay is expected to use rather than
//! emulate. This module is the whole of that adoption.
//!
//! # Why this exists at all: the MCP-only install mode
//!
//! In the plugin-bearing install (the default component set, `Core` +
//! `ContextMcp`), the MCP route is *inside* the bundle — the rendered
//! `.mcp.json` under `.codex/plugins/tracedecay/` declares the `graph` server,
//! and Codex loads it only once the operator has installed and enabled the
//! plugin. TraceDecay must not also register a second, standalone server there:
//! that would give a plugin user two identical tracedecay MCP servers, one of
//! them invisible to `codex plugin` management.
//!
//! An **MCP-only** component set (`ContextMcp` and/or `OperatorMcp` selected
//! *without* `Core`) is the case that had no working registration at all before
//! this module. That set deploys the plugin's `.mcp.json` and nothing else, and
//! since the plugin is never installed, the file is inert — the operator ends
//! up with a staged file and no MCP server. `codex mcp add` is exactly the
//! host-owned command that closes that gap without touching the plugin
//! lifecycle, so it is driven for that set and only for that set. See
//! [`is_mcp_only_component_set`].
//!
//! # Which file this mutates
//!
//! `codex mcp add`/`remove` maintain the `[mcp_servers.<name>]` tables in
//! `~/.codex/config.toml`. That path is already known to this integration:
//! `super::codex_registration_residue` reads `mcp_servers.tracedecay` from it
//! when deciding whether any TraceDecay registration remains. The registration
//! transaction is therefore told about exactly that file after the command
//! runs, so its existing rollback authority can restore the pre-command
//! document. TraceDecay still never *writes* it — Codex's own CLI does.
//!
//! Because that one file also carries Codex-owned activation and hook-trust
//! state, a region guard runs on both sides of the invocation: if the host
//! command changed an operator's peer MCP servers, or any plugin-activation or
//! hook-trust record, the effect is refused instead of accepted. That is the
//! same preservation guard Kiro's registry driver uses, extended to the two
//! regions the owner ruling puts off-limits.

use std::path::{Path, PathBuf};

use crate::agents::host_bundle_v2::HostBundleComponentV1;
use crate::errors::{Result, TraceDecayError};

use super::{CODEX_MCP_SERVER_ARGS, CODEX_MCP_SERVER_ENV, codex_config_path};

/// Name of Codex's own CLI, which owns the MCP registry.
const CODEX_CLI: &str = "codex";

/// What the binary is required *for*, used in the typed absence error.
pub(super) const CODEX_CLI_LIFECYCLE: &str = "codex MCP registry lifecycle";

/// Name Codex's registry selects the server by (`codex mcp add <name>`,
/// `codex mcp remove <name>`) and the key it lands under in `[mcp_servers]`.
///
/// `tracedecay` rather than the plugin bundle's `graph`: the plugin's server key
/// is namespaced by the plugin that ships it, while a standalone registration
/// lands at the top level of the operator's config. It matches the key
/// `super::codex_registration_residue` already looks for, so a standalone
/// registration is correctly reported as residue by the uninstall probe.
pub(super) const CODEX_MCP_SERVER_NAME: &str = "tracedecay";

/// TOML key holding Codex's MCP registry.
const CODEX_MCP_SERVERS_KEY: &str = "mcp_servers";

/// TOML key holding Codex's plugin activation records (`tracedecay@…`). Read
/// only, to prove the registry command left it alone.
const CODEX_PLUGINS_KEY: &str = "plugins";

/// TOML key holding Codex's hook-trust records. Read only, for the same reason.
const CODEX_HOOKS_KEY: &str = "hooks";

/// Whether this component set is the MCP-only (non-plugin) install.
///
/// True exactly when an MCP component is selected and `Core` is not. `Core`
/// carries the Codex plugin bundle — its hooks, skills, and the `.mcp.json`
/// Codex reads once the plugin is installed — so a `Core`-bearing set already
/// has an MCP route and must not gain a second, standalone one. Without `Core`
/// the staged `.mcp.json` is never loaded by anything, and the host registry is
/// the only way the server actually exists.
pub(super) fn is_mcp_only_component_set(components: &[HostBundleComponentV1]) -> bool {
    !components.contains(&HostBundleComponentV1::Core)
        && (components.contains(&HostBundleComponentV1::ContextMcp)
            || components.contains(&HostBundleComponentV1::OperatorMcp))
}

/// Resolve Codex's own CLI, or fail with the typed requirement.
///
/// Codex owns `[mcp_servers]` in `~/.codex/config.toml`. Its CLI is therefore a
/// hard requirement for this lifecycle, not a preference with a config-editing
/// fallback: hand-writing that file is precisely what the owner ruling and the
/// host-capability doctrine forbid, and it is the same file that carries
/// activation and hook-trust state TraceDecay must never author.
pub(super) fn require_codex_cli() -> Result<PathBuf> {
    crate::agents::host_cli::require_host_cli(CODEX_CLI, CODEX_CLI_LIFECYCLE)
}

/// Drive Codex's own registry to add the tracedecay MCP server.
///
/// Split from the trait method so tests can supply a fake CLI and an isolated
/// `HOME` without mutating the process environment.
///
/// The launch contract (`--env` pairs, then `--`, then command and arguments)
/// is built from [`CODEX_MCP_SERVER_ENV`] and [`CODEX_MCP_SERVER_ARGS`] — the
/// same constants the plugin bundle's `.mcp.json` writer consumes, so the two
/// spellings of the same server cannot drift apart.
///
/// The bundle additionally pins `startup_timeout_sec`/`tool_timeout_sec`, which
/// `codex mcp add` exposes no flag for. Those are deliberately *not* emulated by
/// writing the config afterwards: the registered server takes Codex's own
/// defaults, and an operator who wants the bundle's longer bounds installs the
/// plugin. Inventing a post-command edit would be TraceDecay authoring the file
/// Codex owns, which is exactly what this module exists to avoid.
pub(super) fn codex_mcp_add_with(
    codex_cli: &Path,
    home: &Path,
    tracedecay_bin: &str,
) -> Result<()> {
    // Owned first: `args` borrows from this for the length of the call.
    let env_pairs: Vec<String> = CODEX_MCP_SERVER_ENV
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    let mut args = vec!["mcp", "add", CODEX_MCP_SERVER_NAME];
    for pair in &env_pairs {
        args.extend(["--env", pair.as_str()]);
    }
    // Everything after `--` is the server's own launch command, so a launch
    // argument can never be re-read as a `codex mcp add` option.
    args.push("--");
    args.push(tracedecay_bin);
    args.extend(CODEX_MCP_SERVER_ARGS.iter().copied());
    run_codex_mcp_step(codex_cli, &args, home)
}

/// Drive Codex's own registry to drop the tracedecay MCP server.
pub(super) fn codex_mcp_remove_with(codex_cli: &Path, home: &Path) -> Result<()> {
    run_codex_mcp_step(codex_cli, &["mcp", "remove", CODEX_MCP_SERVER_NAME], home)
}

/// Run one `codex mcp ...` step, converting a failed invocation into the host's
/// own diagnosis.
///
/// The region snapshot is a preservation guard: Codex owns the registry merge,
/// but a buggy or changed host command must not be allowed to silently discard
/// an operator's other MCP servers, nor to disturb the plugin-activation and
/// hook-trust records the owner ruling reserves to Codex's interactive flows.
/// The exact post-command bytes are recorded through the active host
/// transaction so its existing rollback authority can restore the pre-command
/// document when the command fails or a later verification step rejects it.
fn run_codex_mcp_step(codex_cli: &Path, args: &[&str], home: &Path) -> Result<()> {
    let config_path = codex_config_path(home);
    let regions_before = preserved_regions(&config_path)?;
    let outcome = crate::agents::host_cli::run_host_cli(codex_cli, args, home)?;
    // Snapshot once after the child exits. The bytes that pass the region guard
    // are the bytes recorded for rollback; reading again after recording would
    // create a race in which a foreign writer could be absorbed into the
    // transaction's intended state and later overwritten during recovery.
    let (observed_bytes, regions_after) = read_config_observation(&config_path)?;
    if regions_before != regions_after {
        let invocation = if args.is_empty() {
            codex_cli.display().to_string()
        } else {
            format!("{} {}", codex_cli.display(), args.join(" "))
        };
        return Err(TraceDecayError::Config {
            message: format!(
                "`{invocation}` changed Codex-owned state in {} that TraceDecay does not author \
                 (peer MCP servers, plugin activation, or hook trust); TraceDecay left the host \
                 state unaccepted",
                config_path.display()
            ),
        });
    }
    crate::agents::record_host_config_observation_bytes(&config_path, observed_bytes.as_deref())?;
    if outcome.succeeded() {
        return Ok(());
    }
    Err(TraceDecayError::Config {
        message: outcome.failure_message(),
    })
}

/// The regions of `~/.codex/config.toml` that a tracedecay MCP registration
/// must leave byte-for-byte semantically identical.
///
/// Not a full-document comparison: the whole point of the invocation is that
/// Codex rewrites `[mcp_servers.tracedecay]`, and Codex is also free to
/// normalise formatting anywhere while doing so. Only the parts TraceDecay has
/// no authority over are compared.
// No `Eq`: `toml::Value` carries floats and is only `PartialEq`.
#[derive(Debug, Default, PartialEq)]
struct CodexPreservedRegionsV1 {
    /// Operator-owned `[mcp_servers.*]` entries other than TraceDecay's.
    mcp_peers: toml::Table,
    /// Codex-owned plugin activation records (`[plugins."tracedecay@…"]` and
    /// every operator peer beside it).
    plugins: Option<toml::Value>,
    /// Codex-owned hook trust records (`[hooks.state."…"]`).
    hooks: Option<toml::Value>,
}

/// Read-only snapshot of the regions the host command must not disturb. The
/// host CLI remains the only writer; this lets the lifecycle reject a command
/// whose effect exceeded what TraceDecay asked for.
fn preserved_regions(path: &Path) -> Result<CodexPreservedRegionsV1> {
    let (_, regions) = read_config_observation(path)?;
    Ok(regions)
}

/// Read the config once, returning both its exact bytes (for the rollback
/// record) and its preserved regions (for the guard). A missing file is a valid
/// observation — Codex creates the config on first `mcp add`.
fn read_config_observation(path: &Path) -> Result<(Option<Vec<u8>>, CodexPreservedRegionsV1)> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((None, CodexPreservedRegionsV1::default()));
        }
        Err(error) => {
            return Err(TraceDecayError::Config {
                message: format!(
                    "failed to read {} before Codex CLI: {error}",
                    path.display()
                ),
            });
        }
    };
    // Borrow for the parse and keep `bytes` owned, so the exact observed bytes
    // are what gets recorded for rollback on every return path.
    let text = std::str::from_utf8(&bytes)
        .map_err(|error| TraceDecayError::Config {
            message: format!("{} is not valid UTF-8: {error}", path.display()),
        })?
        .to_owned();
    if text.trim().is_empty() {
        return Ok((Some(bytes), CodexPreservedRegionsV1::default()));
    }
    let config: toml::Table = toml::from_str(&text).map_err(|error| TraceDecayError::Config {
        message: format!("failed to parse {} as TOML: {error}", path.display()),
    })?;
    let mcp_peers = match config.get(CODEX_MCP_SERVERS_KEY) {
        None => toml::Table::new(),
        Some(servers) => {
            let Some(servers) = servers.as_table() else {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "{}.{CODEX_MCP_SERVERS_KEY} must be a TOML table",
                        path.display()
                    ),
                });
            };
            let mut peers = toml::Table::new();
            for (name, server) in servers {
                if name.as_str() != CODEX_MCP_SERVER_NAME {
                    peers.insert(name.clone(), server.clone());
                }
            }
            peers
        }
    };
    Ok((
        Some(bytes),
        CodexPreservedRegionsV1 {
            mcp_peers,
            plugins: config.get(CODEX_PLUGINS_KEY).cloned(),
            hooks: config.get(CODEX_HOOKS_KEY).cloned(),
        },
    ))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    //! Host-CLI-driven Codex MCP registry lifecycle.
    //!
    //! Codex owns `[mcp_servers]` in `~/.codex/config.toml` through `codex
    //! mcp`, so TraceDecay drives that CLI rather than editing the file. These
    //! tests stand a fake `codex` in an isolated HOME, assert the exact argv
    //! TraceDecay issues, and assert that an absent binary refuses instead of
    //! falling back to config surgery. The fake host emulates the registry's own
    //! effect (add writes the server table, remove drops it) so removal can be
    //! shown to reverse installation rather than merely being spelled correctly.

    use super::*;

    /// Install a fake `codex` that appends each invocation's argv to `log` and
    /// then performs `body`.
    #[cfg(unix)]
    fn fake_codex_cli(bin: &Path, log: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\n{body}\n",
            log = shell_single_quote(&log.to_string_lossy()),
        );
        std::fs::write(bin, script).unwrap();
        let mut permissions = std::fs::metadata(bin).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(bin, permissions).unwrap();
    }

    /// Body for a fake `codex` that emulates the registry's own writes, so a
    /// test can observe that TraceDecay's removal really reverses its install.
    /// A pre-existing peer server and a pre-existing plugin-activation record
    /// are both carried across, exactly as the real command must.
    #[cfg(unix)]
    const FAKE_REGISTRY_BODY: &str = r#"config="$HOME/.codex/config.toml"
peer=''
kept=''
if [ -f "$config" ]; then
  if /bin/grep -q 'mcp_servers.other' "$config"; then
    peer='[mcp_servers.other]
command = "other"
'
  fi
  if /bin/grep -q '^\[plugins' "$config"; then
    kept='[plugins."tracedecay@personal"]
enabled = true
'
  fi
fi
case "$1 $2" in
  "mcp add")
    /bin/mkdir -p "$HOME/.codex"
    printf '%s[mcp_servers.tracedecay]\ncommand = "/bin/tracedecay"\nargs = ["serve"]\n%s' "$peer" "$kept" > "$config"
    ;;
  "mcp remove")
    if [ -n "$peer" ] || [ -n "$kept" ]; then
      printf '%s%s' "$peer" "$kept" > "$config"
    else
      /bin/rm -f "$config"
    fi
    ;;
esac
exit 0"#;

    /// Body for a fake `codex` that succeeds at the registry write but
    /// "forgets" the Codex-owned plugin activation record while rewriting the
    /// document. The lifecycle must refuse that effect rather than accept it.
    #[cfg(unix)]
    const FAKE_DROPS_ACTIVATION_BODY: &str = r#"/bin/mkdir -p "$HOME/.codex"
printf '%s\n' '[mcp_servers.tracedecay]' 'command = "/bin/tracedecay"' > "$HOME/.codex/config.toml"
exit 0"#;

    #[cfg(unix)]
    fn shell_single_quote(value: &str) -> String {
        format!("'{}'", value.replace('\'', r"'\''"))
    }

    /// `[mcp_servers.<name>].command` from a parsed Codex config, or `None`
    /// when the server is absent.
    #[cfg(unix)]
    fn server_command<'a>(config: &'a toml::Table, name: &str) -> Option<&'a str> {
        config
            .get(CODEX_MCP_SERVERS_KEY)?
            .as_table()?
            .get(name)?
            .get("command")?
            .as_str()
    }

    #[cfg(unix)]
    fn recorded_invocations(log: &Path) -> Vec<String> {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// The argv the registered contract must produce, in one place: every
    /// invocation assertion below compares against this exact string.
    #[cfg(unix)]
    const EXPECTED_ADD_INVOCATION: &str =
        "mcp add tracedecay --env TRACEDECAY_ENABLE_GLOBAL_DB=1 -- /bin/tracedecay serve";

    #[cfg(unix)]
    #[test]
    fn activation_drives_codexs_own_mcp_add_with_the_registered_server_contract() {
        let home = tempfile::tempdir().unwrap();
        let bin_dir = tempfile::tempdir().unwrap();
        let log = bin_dir.path().join("invocations.log");
        let codex_cli = bin_dir.path().join("codex");
        fake_codex_cli(&codex_cli, &log, FAKE_REGISTRY_BODY);

        codex_mcp_add_with(&codex_cli, home.path(), "/bin/tracedecay")
            .expect("a clean host CLI run is a completed registration");

        assert_eq!(
            recorded_invocations(&log),
            vec![EXPECTED_ADD_INVOCATION.to_string()],
            "activation must add the server through Codex's own registry, naming it, passing \
             each environment entry as `--env KEY=VALUE`, and separating the launch command \
             with `--`"
        );
        assert!(
            codex_config_path(home.path()).exists(),
            "the host's own registry write must be what lands the entry"
        );
    }

    #[cfg(unix)]
    #[test]
    fn removal_drives_codexs_own_mcp_remove_and_reverses_the_registration() {
        let home = tempfile::tempdir().unwrap();
        let bin_dir = tempfile::tempdir().unwrap();
        let log = bin_dir.path().join("invocations.log");
        let codex_cli = bin_dir.path().join("codex");
        fake_codex_cli(&codex_cli, &log, FAKE_REGISTRY_BODY);
        let config_path = codex_config_path(home.path());
        assert!(
            !config_path.exists(),
            "precondition: nothing registered yet"
        );

        codex_mcp_add_with(&codex_cli, home.path(), "/bin/tracedecay").unwrap();
        codex_mcp_remove_with(&codex_cli, home.path())
            .expect("a clean host CLI run is a completed removal");

        assert_eq!(
            recorded_invocations(&log),
            vec![
                EXPECTED_ADD_INVOCATION.to_string(),
                "mcp remove tracedecay".to_string(),
            ],
            "removal must address the server by the same registry name the add used"
        );
        assert!(
            !config_path.exists(),
            "removal must fully reverse installation, leaving no tracedecay entry behind"
        );
    }

    #[cfg(unix)]
    #[test]
    fn add_and_remove_preserve_an_operator_owned_peer_server() {
        let home = tempfile::tempdir().unwrap();
        let bin_dir = tempfile::tempdir().unwrap();
        let log = bin_dir.path().join("invocations.log");
        let codex_cli = bin_dir.path().join("codex");
        fake_codex_cli(&codex_cli, &log, FAKE_REGISTRY_BODY);
        let config_path = codex_config_path(home.path());
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(
            &config_path,
            "[mcp_servers.other]\ncommand = \"other\"\n\n[mcp_servers.tracedecay]\ncommand = \"/old/tracedecay\"\n",
        )
        .unwrap();

        codex_mcp_add_with(&codex_cli, home.path(), "/bin/tracedecay")
            .expect("host add must update tracedecay while preserving the peer");
        let added: toml::Table =
            toml::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
        assert_eq!(server_command(&added, "other"), Some("other"));
        assert_eq!(
            server_command(&added, "tracedecay"),
            Some("/bin/tracedecay"),
            "the host's own registry write must carry the requested launch command"
        );

        codex_mcp_remove_with(&codex_cli, home.path())
            .expect("host remove must preserve the peer while dropping tracedecay");
        let removed: toml::Table =
            toml::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
        assert_eq!(server_command(&removed, "other"), Some("other"));
        assert_eq!(
            server_command(&removed, "tracedecay"),
            None,
            "removal must drop only TraceDecay's own entry"
        );
    }

    /// The owner ruling reserves plugin activation and hook trust to Codex's
    /// own interactive flows. A registry command that disturbs them exceeds what
    /// TraceDecay asked for and must be refused rather than accepted.
    #[cfg(unix)]
    #[test]
    fn a_registry_command_that_disturbs_codex_owned_activation_state_is_refused() {
        let home = tempfile::tempdir().unwrap();
        let bin_dir = tempfile::tempdir().unwrap();
        let log = bin_dir.path().join("invocations.log");
        let codex_cli = bin_dir.path().join("codex");
        fake_codex_cli(&codex_cli, &log, FAKE_DROPS_ACTIVATION_BODY);
        let config_path = codex_config_path(home.path());
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(
            &config_path,
            "[plugins.\"tracedecay@personal\"]\nenabled = true\n",
        )
        .unwrap();

        let error = codex_mcp_add_with(&codex_cli, home.path(), "/bin/tracedecay")
            .expect_err("a command that drops Codex-owned activation state must not be accepted");

        let TraceDecayError::Config { message } = error else {
            panic!("a rejected host effect must surface as a config error");
        };
        assert!(
            message.contains("plugin activation") && message.contains("unaccepted"),
            "the refusal must name the Codex-owned state it protected: {message}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_failing_codex_registry_command_reports_the_hosts_own_diagnosis() {
        let home = tempfile::tempdir().unwrap();
        let bin_dir = tempfile::tempdir().unwrap();
        let log = bin_dir.path().join("invocations.log");
        let codex_cli = bin_dir.path().join("codex");
        fake_codex_cli(
            &codex_cli,
            &log,
            "echo 'No MCP server named tracedecay is configured' >&2\nexit 7",
        );

        let error = codex_mcp_remove_with(&codex_cli, home.path())
            .expect_err("a non-zero host CLI exit must fail the lifecycle");

        let TraceDecayError::Config { message } = error else {
            panic!("a failed host command must surface as a config error");
        };
        assert!(
            message.contains("No MCP server named tracedecay is configured")
                && message.contains("exit code 7"),
            "the host's own stderr and status must reach the operator: {message}"
        );
    }

    #[test]
    fn a_missing_codex_binary_refuses_instead_of_editing_host_owned_state() {
        let home = tempfile::tempdir().unwrap();
        let config_path = codex_config_path(home.path());
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        // Both an operator's own MCP server and the Codex-owned activation and
        // hook-trust records the ruling puts off-limits.
        let operator_owned = concat!(
            "[mcp_servers.someone-elses]\ncommand = \"other\"\n\n",
            "[plugins.\"tracedecay@personal\"]\nenabled = true\n\n",
            "[hooks.state.\"tracedecay@personal:session_start:0:0\"]\ntrusted_hash = \"sha256:abc\"\n",
        );
        std::fs::write(&config_path, operator_owned).unwrap();

        let error = crate::agents::host_cli::require_host_cli(
            "codex-definitely-absent",
            CODEX_CLI_LIFECYCLE,
        )
        .expect_err("an absent host binary is a hard requirement failure");

        let TraceDecayError::HostCliUnavailable { program, lifecycle } = error else {
            panic!("host CLI absence must surface as a typed requirement");
        };
        assert_eq!(program, "codex-definitely-absent");
        assert_eq!(lifecycle, CODEX_CLI_LIFECYCLE);
        assert_eq!(
            std::fs::read_to_string(&config_path).unwrap(),
            operator_owned,
            "a refused lifecycle must not have touched host-owned Codex config state"
        );
        assert!(
            !crate::agents::config_backup_path(&config_path).exists(),
            "a refused lifecycle must not have staged a backup of host-owned config state"
        );
    }

    /// Only the non-plugin set drives the MCP registry. A `Core`-bearing set
    /// already carries its MCP route inside the plugin bundle, whose install
    /// is driven by [`super::plugin_registry`].
    #[test]
    fn only_the_non_plugin_component_set_drives_the_registry() {
        use HostBundleComponentV1::{ContextMcp, Core, OperatorMcp};

        assert!(is_mcp_only_component_set(&[ContextMcp]));
        assert!(is_mcp_only_component_set(&[OperatorMcp]));
        assert!(is_mcp_only_component_set(&[ContextMcp, OperatorMcp]));
        assert!(
            !is_mcp_only_component_set(&[Core, ContextMcp]),
            "the default plugin-bearing set must not gain a second standalone server"
        );
        assert!(!is_mcp_only_component_set(&[Core]));
        assert!(!is_mcp_only_component_set(&[]));
    }

    /// The CLI invocation and the plugin bundle's `.mcp.json` writer must launch
    /// the same server the same way; both read the shared constants.
    #[test]
    fn the_cli_launch_contract_matches_the_plugin_bundle_mcp_writer() {
        let rendered = super::super::rendered_global_plugin_files("/bin/tracedecay")
            .expect("the global Codex bundle must render");
        let mcp = rendered
            .iter()
            .find_map(|(relative, body)| (*relative == ".mcp.json").then_some(body))
            .expect("the global Codex bundle ships .mcp.json");
        let mcp: serde_json::Value = serde_json::from_str(mcp).unwrap();
        let server = &mcp["mcpServers"]["graph"];

        assert_eq!(
            server["args"],
            serde_json::to_value(CODEX_MCP_SERVER_ARGS).unwrap(),
            "the CLI's post-`--` launch arguments and the bundle writer's args must match"
        );
        for (key, value) in CODEX_MCP_SERVER_ENV {
            assert_eq!(
                server["env"][*key].as_str(),
                Some(*value),
                "the CLI's `--env {key}={value}` and the bundle writer's env must match"
            );
        }
    }
}
