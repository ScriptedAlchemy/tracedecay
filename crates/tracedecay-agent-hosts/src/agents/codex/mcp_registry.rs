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
//! `ContextMcp`), the MCP route is *inside* the bundle, the rendered
//! `.mcp.json` under `.codex/plugins/tracedecay/` declares the `graph` server,
//! and Codex loads it only once the operator has installed and enabled the
//! plugin. TraceDecay must not also register a second, standalone server there:
//! that would give a plugin user two identical tracedecay MCP servers, one of
//! them invisible to `codex plugin` management.
//!
//! An **MCP-only** component set (`ContextMcp` and/or `OperatorMcp` selected
//! *without* `Core`) is the case that had no working registration at all before
//! this module. That set deploys the plugin's `.mcp.json` and nothing else, and
//! since the plugin is never installed, the file is inert, the operator ends
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
//! document. TraceDecay still never *writes* it. Codex's own CLI does.
//!
//! Because that one file also carries Codex-owned activation and hook-trust
//! state, a region guard runs on both sides of the invocation: if the host
//! command changed an operator's peer MCP servers, or any plugin-activation or
//! hook-trust record, the effect is refused instead of accepted. That is the
//! same preservation guard Kiro's registry driver uses, extended to the two
//! regions the owner ruling puts off-limits.

use std::path::{Path, PathBuf};

use crate::agents::host_bundle::HostComponentV1;
use tracedecay_domain::errors::{Result, TraceDecayError};

use super::{CODEX_SERVE_TAIL, codex_config_path};
use crate::agents::serve_args::ServeArgs;

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
pub(super) const CODEX_MCP_SERVERS_KEY: &str = "mcp_servers";

/// TOML key holding Codex's plugin activation records (`tracedecay@…`). Read
/// only, to prove the registry command left it alone.
pub(super) const CODEX_PLUGINS_KEY: &str = "plugins";

/// TOML key holding Codex's hook-trust records. Read only, for the same reason.
/// The plugin CLI must never disturb them either; TraceDecay writes its own
/// managed entries through [`super::sync_codex_hook_trust`], outside any CLI run.
pub(super) const CODEX_HOOKS_KEY: &str = "hooks";

/// Whether this component set is the MCP-only (non-plugin) install.
///
/// True exactly when an MCP component is selected and `Core` is not. `Core`
/// carries the Codex plugin bundle, its hooks, skills, and the `.mcp.json`
/// Codex reads once the plugin is installed, so a `Core`-bearing set already
/// has an MCP route and must not gain a second, standalone one. Without `Core`
/// the staged `.mcp.json` is never loaded by anything, and the host registry is
/// the only way the server actually exists.
pub(super) fn is_mcp_only_component_set(components: &[HostComponentV1]) -> bool {
    !components.contains(&HostComponentV1::Core)
        && (components.contains(&HostComponentV1::ContextMcp)
            || components.contains(&HostComponentV1::OperatorMcp))
}

/// Whether `component` is an MCP route served by Codex's standalone registry
/// server, launching `tracedecay_bin` when one is given with exactly
/// `serve_args`. This is the MCP-only install's readback: that install never
/// activates the plugin.
pub(super) fn standalone_mcp_route(
    component: HostComponentV1,
    home: &Path,
    tracedecay_bin: Option<&str>,
    serve_args: &ServeArgs,
) -> std::result::Result<bool, ()> {
    if !matches!(
        component,
        HostComponentV1::ContextMcp | HostComponentV1::OperatorMcp
    ) {
        return Ok(false);
    }
    let Some(server) = standalone_server(home).map_err(drop)? else {
        return Ok(false);
    };
    let args_match = server
        .get("args")
        .and_then(toml::Value::as_array)
        .and_then(|args| {
            args.iter()
                .map(toml::Value::as_str)
                .collect::<Option<Vec<_>>>()
        })
        .is_some_and(|args| serve_args.matches(&args, CODEX_SERVE_TAIL));
    Ok(args_match
        && tracedecay_bin
            .is_none_or(|bin| server.get("command").and_then(toml::Value::as_str) == Some(bin)))
}

fn standalone_server(home: &Path) -> Result<Option<toml::Value>> {
    let (_, mut config) = read_config_table(&codex_config_path(home))?;
    Ok(config
        .remove(CODEX_MCP_SERVERS_KEY)
        .and_then(|mut servers| servers.as_table_mut()?.remove(CODEX_MCP_SERVER_NAME)))
}

/// Drop the standalone server an MCP-only install registered, through Codex's
/// own registry, when `components` is the plugin-bearing set. That plugin
/// carries the MCP route, so a standalone server beside it would be a second
/// one, and after its uninstall the server would be registration residue.
pub(super) fn retire_standalone_server(components: &[HostComponentV1], home: &Path) -> Result<()> {
    let plugin_carries_mcp = components.contains(&HostComponentV1::Core)
        && (components.contains(&HostComponentV1::ContextMcp)
            || components.contains(&HostComponentV1::OperatorMcp));
    if !plugin_carries_mcp || standalone_server(home)?.is_none() {
        return Ok(());
    }
    codex_mcp_remove_with(&require_codex_cli()?, home)
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
/// The launch contract (`--`, then command and arguments) is built from the
/// [`ServeArgs`] launch args and [`CODEX_SERVE_TAIL`], the same values the
/// plugin bundle's `.mcp.json` writer consumes, so the two spellings of the
/// same server cannot drift apart.
///
/// The bundle also pins `startup_timeout_sec`/`tool_timeout_sec`, which
/// `codex mcp add` exposes no flag for. Those are deliberately *not* emulated by
/// writing the config afterwards: the registered server takes Codex's own
/// defaults, and an operator who wants the bundle's longer bounds installs the
/// plugin. Inventing a post-command edit would be TraceDecay authoring the file
/// Codex owns, which is exactly what this module exists to avoid.
pub(super) fn codex_mcp_add_with(
    codex_cli: &Path,
    home: &Path,
    tracedecay_bin: &str,
    serve_args: &ServeArgs,
) -> Result<()> {
    let mut args = vec!["mcp", "add", CODEX_MCP_SERVER_NAME];
    // Everything after `--` is the server's own launch command, so a launch
    // argument can never be re-read as a `codex mcp add` option.
    args.push("--");
    args.push(tracedecay_bin);
    let launch = serve_args.to_vec(CODEX_SERVE_TAIL);
    args.extend(launch.iter().map(String::as_str));
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
#[tracing::instrument(
    name = "hosts.agent.codex.mcp_registry_step",
    level = "trace",
    skip_all
)]
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
/// observation. Codex creates the config on first `mcp add`.
fn read_config_observation(path: &Path) -> Result<(Option<Vec<u8>>, CodexPreservedRegionsV1)> {
    let (bytes, config) = read_config_table(path)?;
    Ok((
        bytes,
        CodexPreservedRegionsV1 {
            mcp_peers: table_peers(&config, CODEX_MCP_SERVERS_KEY, CODEX_MCP_SERVER_NAME, path)?,
            plugins: config.get(CODEX_PLUGINS_KEY).cloned(),
            hooks: config.get(CODEX_HOOKS_KEY).cloned(),
        },
    ))
}

/// Read the Codex config once, returning its exact bytes (for the rollback
/// record) and its parsed table. A missing file has no bytes; a missing or
/// blank file parses as an empty table.
pub(super) fn read_config_table(path: &Path) -> Result<(Option<Vec<u8>>, toml::Table)> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((None, toml::Table::new()));
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
    let text = std::str::from_utf8(&bytes).map_err(|error| TraceDecayError::Config {
        message: format!("{} is not valid UTF-8: {error}", path.display()),
    })?;
    if text.trim().is_empty() {
        return Ok((Some(bytes), toml::Table::new()));
    }
    let config = toml::from_str(text).map_err(|error| TraceDecayError::Config {
        message: format!("failed to parse {} as TOML: {error}", path.display()),
    })?;
    Ok((Some(bytes), config))
}

/// The entries of the `key` table other than TraceDecay's own `owned` entry.
pub(super) fn table_peers(
    config: &toml::Table,
    key: &str,
    owned: &str,
    path: &Path,
) -> Result<toml::Table> {
    let Some(entries) = config.get(key) else {
        return Ok(toml::Table::new());
    };
    let Some(entries) = entries.as_table() else {
        return Err(TraceDecayError::Config {
            message: format!("{}.{key} must be a TOML table", path.display()),
        });
    };
    Ok(entries
        .iter()
        .filter(|(name, _)| name.as_str() != owned)
        .map(|(name, entry)| (name.clone(), entry.clone()))
        .collect())
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
    #[cfg(unix)]
    use tracedecay_runtime_core::test_executable::write_executable_script;

    /// A standalone server on another catalog profile is not current, so an
    /// MCP-only install that switches profiles re-registers it.
    #[test]
    fn the_standalone_readback_requires_the_selected_catalog_profile() {
        let home = tempfile::tempdir().unwrap();
        let config_path = codex_config_path(home.path());
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(
            &config_path,
            "[mcp_servers.tracedecay]\ncommand = \"/bin/td\"\nargs = [\"serve\", \"--full-tool-list\"]\n",
        )
        .unwrap();
        let catalog =
            tracedecay_contracts::catalog_composition::build_application_binding_snapshot()
                .unwrap();
        let compact = ServeArgs::from(
            tracedecay_contracts::catalog_composition::CatalogProfileName::parse(
                &catalog, "compact",
            )
            .unwrap(),
        );
        let route = |serve_args: &ServeArgs| {
            standalone_mcp_route(
                HostComponentV1::ContextMcp,
                home.path(),
                Some("/bin/td"),
                serve_args,
            )
            .unwrap()
        };
        assert!(route(&ServeArgs::default()));
        assert!(!route(&compact));

        std::fs::write(
            &config_path,
            "[mcp_servers.tracedecay]\ncommand = \"/bin/td\"\nargs = [\"serve\"]\n",
        )
        .unwrap();
        assert!(
            !route(&ServeArgs::default()),
            "a server registered without --full-tool-list needs repair"
        );
    }

    /// Install a fake `codex` that appends each invocation's argv to `log` and
    /// then performs `body`.
    #[cfg(unix)]
    fn fake_codex_cli(bin: &Path, log: &Path, body: &str) {
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\n{body}\n",
            log = shell_single_quote(&log.to_string_lossy()),
        );
        write_executable_script(bin, script).unwrap();
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
  if /usr/bin/grep -q 'mcp_servers.other' "$config"; then
    peer='[mcp_servers.other]
command = "other"
'
  fi
  if /usr/bin/grep -q '^\[plugins' "$config"; then
    kept='[plugins."tracedecay@personal"]
enabled = true
'
  fi
fi
case "$1 $2" in
  "mcp add")
    /bin/mkdir -p "$HOME/.codex"
    printf '%s[mcp_servers.tracedecay]\ncommand = "/bin/tracedecay"\nargs = ["serve", "--full-tool-list"]\n%s' "$peer" "$kept" > "$config"
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
        "mcp add tracedecay -- /bin/tracedecay serve --full-tool-list";

    #[cfg(unix)]
    #[test]
    fn activation_drives_codexs_own_mcp_add_with_the_registered_server_contract() {
        let home = tempfile::tempdir().unwrap();
        let bin_dir = tempfile::tempdir().unwrap();
        let log = bin_dir.path().join("invocations.log");
        let codex_cli = bin_dir.path().join("codex");
        fake_codex_cli(&codex_cli, &log, FAKE_REGISTRY_BODY);

        codex_mcp_add_with(
            &codex_cli,
            home.path(),
            "/bin/tracedecay",
            &ServeArgs::default(),
        )
        .expect("a clean host CLI run is a completed registration");

        assert_eq!(
            recorded_invocations(&log),
            vec![EXPECTED_ADD_INVOCATION.to_string()],
            "activation must add the server through Codex's own registry, naming it and \
             separating the launch command with `--`"
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

        codex_mcp_add_with(
            &codex_cli,
            home.path(),
            "/bin/tracedecay",
            &ServeArgs::default(),
        )
        .unwrap();
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

        codex_mcp_add_with(
            &codex_cli,
            home.path(),
            "/bin/tracedecay",
            &ServeArgs::default(),
        )
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

        let error = codex_mcp_add_with(
            &codex_cli,
            home.path(),
            "/bin/tracedecay",
            &ServeArgs::default(),
        )
        .expect_err("a command that drops Codex-owned activation state must not be accepted");

        let TraceDecayError::Config { message } = error else {
            panic!("a rejected host effect must surface as a config error");
        };
        assert!(
            message.contains("plugin activation") && message.contains("unaccepted"),
            "the refusal must name the Codex-owned state it protected: {message}"
        );
    }

    /// Body for a fake `codex` whose registry rewrites only the
    /// `[mcp_servers.tracedecay]` table and keeps every other byte in place.
    #[cfg(unix)]
    const FAKE_TABLE_SCOPED_REGISTRY_BODY: &str = r#"config="$HOME/.codex/config.toml"
/bin/mkdir -p "$HOME/.codex"
: >> "$config"
/usr/bin/awk '/^\[/ { owned = ($0 == "[mcp_servers.tracedecay]") } !owned' "$config" > "$config.next"
if [ "$1 $2" = "mcp add" ]; then
  printf '[mcp_servers.tracedecay]\ncommand = "%s"\nargs = ["serve", "--full-tool-list"]\n' "$5" >> "$config.next"
fi
/bin/mv "$config.next" "$config"
exit 0"#;

    /// One receipt-backed component-set transaction for a Codex selection,
    /// through the production registration authority.
    #[cfg(unix)]
    fn run_codex_selection(
        home: &Path,
        lifecycle: &Path,
        selection: &[HostComponentV1],
        operation: crate::agents::host_bundle::HostBundleLifecycleOpV1,
        operation_id: u8,
        tracedecay_bin: &str,
    ) -> std::result::Result<(), crate::agents::host_bundle::HostBundleError> {
        use crate::agents::host_bundle::{
            HostBundleWriterV1, HostComponentSetExecutionRequestV1,
            HostComponentSetLifecycleRequestV1, HostComponentSetTransactionV1, HostKindV1,
        };

        let selected =
            crate::agents::host_bundle_registry::verified_embedded_host_component_set_with_tracedecay_bin(
                HostKindV1::Codex,
                selection,
                0,
                tracedecay_bin,
                crate::agents::TEST_GENERATOR_COMMIT, &crate::agents::serve_args::ServeArgs::default(),)
            .unwrap();
        let request = HostComponentSetExecutionRequestV1 {
            lifecycle: HostComponentSetLifecycleRequestV1 {
                operation,
                expected_host: HostKindV1::Codex,
                expected_components: selected
                    .component_set
                    .components
                    .iter()
                    .map(|component| component.manifest.component)
                    .collect(),
                explicit_confirmation: true,
                hermes_profile_bindings: 0,
                explicit_adoption: false,
            },
            operation_id: [operation_id; 16],
        };
        let mut writer = HostBundleWriterV1::open_with_lifecycle_root(home, lifecycle).unwrap();
        let mut registration =
            crate::agents::host_component_registration::CatalogHostComponentRegistrationAuthority::new_with_tracedecay_bin(
                &tracedecay_runtime_core::config::ProfileRoot::under_home(home),
                "codex",
                home,
                operation,
                tracedecay_bin.to_string(),
            )
            .unwrap();
        HostComponentSetTransactionV1::new(&mut writer)
            .execute(
                &selected.component_set,
                &request,
                &selected,
                &mut registration,
            )
            .map(drop)
    }

    #[cfg(unix)]
    fn tracedecay_servers(config_path: &Path) -> Vec<String> {
        let Ok(text) = std::fs::read_to_string(config_path) else {
            return Vec::new();
        };
        let config: toml::Table = toml::from_str(&text).unwrap();
        config
            .get(CODEX_MCP_SERVERS_KEY)
            .and_then(toml::Value::as_table)
            .map_or_default(|servers| servers.keys().cloned().collect())
    }

    /// A fresh MCP-only install ends with exactly one standalone server that
    /// launches the installed binary, beside the operator's own servers, and
    /// its uninstall removes only that server.
    #[cfg(unix)]
    #[test]
    fn mcp_only_lifecycle_owns_exactly_one_standalone_server() {
        use crate::agents::host_bundle::HostBundleLifecycleOpV1;

        const BIN: &str = "/opt/tracedecay/bin/tracedecay";
        let home = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        let bin_dir = tempfile::tempdir().unwrap();
        fake_codex_cli(
            &bin_dir.path().join("codex"),
            &bin_dir.path().join("invocations.log"),
            FAKE_TABLE_SCOPED_REGISTRY_BODY,
        );
        let _codex_cli =
            tracedecay_runtime_core::config::HostProgramSearchPathGuard::set(bin_dir.path());
        let config_path = codex_config_path(home.path());
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        let operator = "[mcp_servers.other]\ncommand = \"other\"\n\n";
        std::fs::write(&config_path, operator).unwrap();

        run_codex_selection(
            home.path(),
            lifecycle.path(),
            &[HostComponentV1::ContextMcp],
            HostBundleLifecycleOpV1::Install,
            50,
            BIN,
        )
        .expect("a fresh MCP-only install verifies its standalone server");

        assert_eq!(tracedecay_servers(&config_path), ["other", "tracedecay"]);
        let config: toml::Table =
            toml::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
        assert_eq!(server_command(&config, "tracedecay"), Some(BIN));

        run_codex_selection(
            home.path(),
            lifecycle.path(),
            &[HostComponentV1::ContextMcp],
            HostBundleLifecycleOpV1::Uninstall,
            51,
            BIN,
        )
        .expect("the MCP-only uninstall verifies the server is gone");

        assert_eq!(std::fs::read_to_string(&config_path).unwrap(), operator);
    }

    #[cfg(unix)]
    fn live_receipt(
        lifecycle: &Path,
        component: HostComponentV1,
    ) -> Option<crate::agents::host_bundle::HostBundleInstallReceiptV1> {
        crate::agents::host_bundle::latest_host_component_receipt_at(
            lifecycle,
            crate::agents::host_bundle::HostKindV1::Codex,
            component,
        )
        .unwrap()
        .filter(|receipt| {
            receipt.operation != crate::agents::host_bundle::HostBundleLifecycleOpV1::Uninstall
        })
    }

    /// A released binary's MCP-only `operator-mcp` install registered the
    /// standalone server under the legacy identity. The canonical update
    /// re-registers that one server under `context-mcp`, and the canonical
    /// uninstall removes it.
    #[cfg(unix)]
    #[test]
    fn legacy_mcp_only_install_updates_and_uninstalls_as_one_server() {
        use crate::agents::host_bundle::HostBundleLifecycleOpV1;

        const OLD_BIN: &str = "/opt/tracedecay-old/bin/tracedecay";
        const NEW_BIN: &str = "/opt/tracedecay-new/bin/tracedecay";
        let home = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        let bin_dir = tempfile::tempdir().unwrap();
        fake_codex_cli(
            &bin_dir.path().join("codex"),
            &bin_dir.path().join("invocations.log"),
            FAKE_TABLE_SCOPED_REGISTRY_BODY,
        );
        let _codex_cli =
            tracedecay_runtime_core::config::HostProgramSearchPathGuard::set(bin_dir.path());
        let config_path = codex_config_path(home.path());
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        let operator = "[mcp_servers.other]\ncommand = \"other\"\n\n";
        std::fs::write(&config_path, operator).unwrap();

        run_codex_selection(
            home.path(),
            lifecycle.path(),
            &[HostComponentV1::OperatorMcp],
            HostBundleLifecycleOpV1::Install,
            60,
            OLD_BIN,
        )
        .unwrap();
        assert!(live_receipt(lifecycle.path(), HostComponentV1::OperatorMcp).is_some());

        run_codex_selection(
            home.path(),
            lifecycle.path(),
            &[HostComponentV1::ContextMcp],
            HostBundleLifecycleOpV1::Update,
            61,
            NEW_BIN,
        )
        .expect("the canonical update takes over the legacy standalone server");

        assert_eq!(tracedecay_servers(&config_path), ["other", "tracedecay"]);
        let config: toml::Table =
            toml::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
        assert_eq!(server_command(&config, "tracedecay"), Some(NEW_BIN));
        assert!(live_receipt(lifecycle.path(), HostComponentV1::OperatorMcp).is_none());
        assert!(live_receipt(lifecycle.path(), HostComponentV1::ContextMcp).is_some());

        run_codex_selection(
            home.path(),
            lifecycle.path(),
            &[HostComponentV1::ContextMcp],
            HostBundleLifecycleOpV1::Uninstall,
            62,
            NEW_BIN,
        )
        .expect("the canonical uninstall removes the migrated server");

        assert_eq!(std::fs::read_to_string(&config_path).unwrap(), operator);
        assert!(live_receipt(lifecycle.path(), HostComponentV1::ContextMcp).is_none());
    }

    /// Body for a fake `codex` whose `mcp add` writes the tracedecay server and
    /// also drops every table whose header starts with `region`.
    #[cfg(unix)]
    fn fake_registry_dropping(region: &str) -> String {
        format!(
            r#"config="$HOME/.codex/config.toml"
/usr/bin/awk '/^\[/ {{ dropped = index($0, "{region}") == 1 }} !dropped' "$config" > "$config.next"
printf '[mcp_servers.tracedecay]\ncommand = "%s"\n' "$5" >> "$config.next"
/bin/mv "$config.next" "$config"
exit 0"#
        )
    }

    /// When Codex's own `mcp add` disturbs an operator peer server, plugin
    /// activation or hook trust, the MCP-only install refuses and rolls back to
    /// the operator's exact config, deploys nothing and records no owner.
    #[cfg(unix)]
    #[test]
    fn refused_mcp_only_install_restores_only_what_it_changed() {
        use crate::agents::host_bundle::HostBundleLifecycleOpV1;

        let operator = "[mcp_servers.other]\ncommand = \"other\"\n\n\
                        [plugins.\"other@personal\"]\nenabled = true\n\n\
                        [hooks.state.\"other@personal:hooks/hooks.json:stop:0:0\"]\n\
                        trusted_hash = \"sha256:00\"\n";
        for region in ["[mcp_servers.other]", "[plugins.", "[hooks."] {
            let home = tempfile::tempdir().unwrap();
            let lifecycle = tempfile::tempdir().unwrap();
            let bin_dir = tempfile::tempdir().unwrap();
            fake_codex_cli(
                &bin_dir.path().join("codex"),
                &bin_dir.path().join("invocations.log"),
                &fake_registry_dropping(region),
            );
            let _codex_cli =
                tracedecay_runtime_core::config::HostProgramSearchPathGuard::set(bin_dir.path());
            let config_path = codex_config_path(home.path());
            std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
            std::fs::write(&config_path, operator).unwrap();

            run_codex_selection(
                home.path(),
                lifecycle.path(),
                &[HostComponentV1::ContextMcp],
                HostBundleLifecycleOpV1::Install,
                70,
                "/opt/tracedecay/bin/tracedecay",
            )
            .expect_err("a host command that disturbs Codex-owned state is refused");

            assert_eq!(
                std::fs::read_to_string(&config_path).unwrap(),
                operator,
                "{region}"
            );
            assert!(
                !home
                    .path()
                    .join(".codex/plugins/tracedecay/.mcp.json")
                    .exists(),
                "{region}"
            );
            assert!(
                live_receipt(lifecycle.path(), HostComponentV1::ContextMcp).is_none(),
                "{region}"
            );
        }
    }

    /// A plugin-bearing install over an MCP-only one retires the standalone
    /// server before `codex plugin add`. When that activation fails, the
    /// refusal restores the standalone server and the operator's peer server
    /// byte for byte and records no owner.
    #[cfg(unix)]
    #[test]
    fn refused_plugin_bearing_install_restores_the_retired_standalone_server() {
        use crate::agents::host_bundle::HostBundleLifecycleOpV1;

        let home = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        let bin_dir = tempfile::tempdir().unwrap();
        let log = bin_dir.path().join("invocations.log");
        fake_codex_cli(
            &bin_dir.path().join("codex"),
            &log,
            &format!(
                "if [ \"$1 $2\" = \"plugin add\" ]; then exit 1; fi\n{FAKE_TABLE_SCOPED_REGISTRY_BODY}"
            ),
        );
        let _codex_cli =
            tracedecay_runtime_core::config::HostProgramSearchPathGuard::set(bin_dir.path());
        let config_path = codex_config_path(home.path());
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        let operator = "[mcp_servers.other]\ncommand = \"other\"\n\n\
                        [mcp_servers.tracedecay]\ncommand = \"/opt/tracedecay/bin/tracedecay\"\n\
                        args = [\"serve\"]\n";
        std::fs::write(&config_path, operator).unwrap();

        run_codex_selection(
            home.path(),
            lifecycle.path(),
            &[HostComponentV1::Core, HostComponentV1::ContextMcp],
            HostBundleLifecycleOpV1::Install,
            80,
            "/opt/tracedecay/bin/tracedecay",
        )
        .expect_err("a failed plugin activation refuses the install");

        let invocations = recorded_invocations(&log);
        assert_eq!(
            invocations.first().map(String::as_str),
            Some("mcp remove tracedecay")
        );
        assert!(
            invocations
                .iter()
                .any(|call| call.starts_with("plugin add"))
        );
        assert_eq!(std::fs::read_to_string(&config_path).unwrap(), operator);
        assert!(live_receipt(lifecycle.path(), HostComponentV1::ContextMcp).is_none());
        assert!(live_receipt(lifecycle.path(), HostComponentV1::Core).is_none());
    }
}
