// Rust guideline compliant 2026-08-14
//! Codex's own non-interactive plugin registry, driven for Core activation.
//!
//! # What shipped, and what this module adopts
//!
//! Plan 27's 2026-08-08 Codex verdict treated plugin activation as
//! interactive-only (reason code `(a)`): `/plugin marketplace add` and
//! `/plugin install` ran inside a session, so TraceDecay staged the source
//! and stopped. Codex CLI 0.147.0 now publishes a non-interactive counterpart:
//!
//! ```text
//! codex plugin add <PLUGIN@MARKETPLACE> [--json]
//! codex plugin remove <PLUGIN@MARKETPLACE> [--json]
//! ```
//!
//! Isolated-HOME probe on 2026-08-14: `codex plugin add tracedecay@personal
//! --json` exits 0 without a TTY, writes `[plugins."tracedecay@personal"]
//! enabled = true` into `~/.codex/config.toml`, and copies the staged source
//! into `~/.codex/plugins/cache/personal/tracedecay/<version>`. It does **not**
//! write `[hooks.state]` — hook trust stays Codex-owned via `/hooks`.
//!
//! That is the same host-capability shape already adopted for `codex mcp add`
//! in [`super::mcp_registry`]: TraceDecay never authors `config.toml`; it
//! drives Codex's own command and records the exact post-command bytes for
//! rollback. The MCP-only / Core split is unchanged: a `Core`-bearing set
//! takes this plugin path, and an MCP-only set still uses the MCP registry
//! so a plugin user does not also get a standalone `tracedecay` server.
//!
//! # Region guard
//!
//! `codex plugin add` is *supposed* to change `plugins.tracedecay@<marketplace>`.
//! Everything else in `config.toml` is off-limits: peer plugin activation,
//! `[mcp_servers]`, and `[hooks]` (including trust hashes). A command that
//! disturbs those regions is refused rather than accepted.

use std::path::{Path, PathBuf};

use crate::errors::{Result, TraceDecayError};

use super::codex_config_path;

/// Name of Codex's own CLI, which owns plugin cache and activation.
const CODEX_CLI: &str = "codex";

/// What the binary is required *for*, used in the typed absence error.
pub(super) const CODEX_PLUGIN_CLI_LIFECYCLE: &str = "codex plugin lifecycle";

/// TOML key holding Codex's plugin activation records (`tracedecay@…`).
const CODEX_PLUGINS_KEY: &str = "plugins";

/// TOML key holding Codex's MCP registry. Read only: this module must not
/// accept a plugin command that also rewrites MCP servers.
const CODEX_MCP_SERVERS_KEY: &str = "mcp_servers";

/// TOML key holding Codex's hook-trust records. Read only: `/hooks` stays
/// Codex-owned even after plugin add succeeds.
const CODEX_HOOKS_KEY: &str = "hooks";

/// Resolve Codex's own CLI, or fail with the typed requirement.
pub(super) fn require_codex_plugin_cli() -> Result<PathBuf> {
    crate::agents::host_cli::require_host_cli(CODEX_CLI, CODEX_PLUGIN_CLI_LIFECYCLE)
}

/// Drive Codex's own registry to install and enable the staged plugin.
///
/// Split from the trait method so tests can supply a fake CLI and an isolated
/// `HOME` without mutating the process environment.
pub(super) fn codex_plugin_add_with(
    codex_cli: &Path,
    home: &Path,
    marketplace_name: &str,
) -> Result<()> {
    let selector = plugin_selector(marketplace_name);
    run_codex_plugin_step(
        codex_cli,
        &["plugin", "add", selector.as_str(), "--json"],
        home,
        marketplace_name,
    )
}

/// Drive Codex's own registry to drop the installed plugin.
pub(super) fn codex_plugin_remove_with(
    codex_cli: &Path,
    home: &Path,
    marketplace_name: &str,
) -> Result<()> {
    let selector = plugin_selector(marketplace_name);
    run_codex_plugin_step(
        codex_cli,
        &["plugin", "remove", selector.as_str(), "--json"],
        home,
        marketplace_name,
    )
}

fn plugin_selector(marketplace_name: &str) -> String {
    format!("tracedecay@{marketplace_name}")
}

/// Run one `codex plugin ...` step, converting a failed invocation into the
/// host's own diagnosis.
///
/// The region snapshot is a preservation guard: Codex owns the merge, but a
/// buggy or changed host command must not discard an operator's other plugins,
/// MCP servers, or hook-trust records. The exact post-command bytes are
/// recorded through the active host transaction so rollback can restore the
/// pre-command document when the command fails or a later step rejects it.
fn run_codex_plugin_step(
    codex_cli: &Path,
    args: &[&str],
    home: &Path,
    marketplace_name: &str,
) -> Result<()> {
    let config_path = codex_config_path(home);
    let owned_key = plugin_selector(marketplace_name);
    let regions_before = preserved_regions(&config_path, &owned_key)?;
    let outcome = crate::agents::host_cli::run_host_cli(codex_cli, args, home)?;
    let (observed_bytes, regions_after) = read_config_observation(&config_path, &owned_key)?;
    if regions_before != regions_after {
        let invocation = if args.is_empty() {
            codex_cli.display().to_string()
        } else {
            format!("{} {}", codex_cli.display(), args.join(" "))
        };
        return Err(TraceDecayError::Config {
            message: format!(
                "`{invocation}` changed Codex-owned state in {} that TraceDecay does not author \
                 (peer plugins, MCP servers, or hook trust); TraceDecay left the host \
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

/// The regions of `~/.codex/config.toml` that a tracedecay plugin registration
/// must leave byte-for-byte semantically identical.
// No `Eq`: `toml::Value` carries floats and is only `PartialEq`.
#[derive(Debug, Default, PartialEq)]
struct CodexPluginPreservedRegionsV1 {
    /// Operator-owned `[plugins.*]` entries other than TraceDecay's selector.
    plugin_peers: toml::Table,
    /// Codex-owned MCP registry (`[mcp_servers]`), including any standalone
    /// tracedecay server from the MCP-only install.
    mcp_servers: Option<toml::Value>,
    /// Codex-owned hook trust records (`[hooks.state."…"]`).
    hooks: Option<toml::Value>,
}

fn preserved_regions(path: &Path, owned_key: &str) -> Result<CodexPluginPreservedRegionsV1> {
    let (_, regions) = read_config_observation(path, owned_key)?;
    Ok(regions)
}

fn read_config_observation(
    path: &Path,
    owned_key: &str,
) -> Result<(Option<Vec<u8>>, CodexPluginPreservedRegionsV1)> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((None, CodexPluginPreservedRegionsV1::default()));
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
    let text = std::str::from_utf8(&bytes)
        .map_err(|error| TraceDecayError::Config {
            message: format!("{} is not valid UTF-8: {error}", path.display()),
        })?
        .to_owned();
    if text.trim().is_empty() {
        return Ok((Some(bytes), CodexPluginPreservedRegionsV1::default()));
    }
    let config: toml::Table = toml::from_str(&text).map_err(|error| TraceDecayError::Config {
        message: format!("failed to parse {} as TOML: {error}", path.display()),
    })?;
    let plugin_peers = match config.get(CODEX_PLUGINS_KEY) {
        None => toml::Table::new(),
        Some(plugins) => {
            let Some(plugins) = plugins.as_table() else {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "{}.{CODEX_PLUGINS_KEY} must be a TOML table",
                        path.display()
                    ),
                });
            };
            let mut peers = toml::Table::new();
            for (name, plugin) in plugins {
                if name.as_str() != owned_key {
                    peers.insert(name.clone(), plugin.clone());
                }
            }
            peers
        }
    };
    Ok((
        Some(bytes),
        CodexPluginPreservedRegionsV1 {
            plugin_peers,
            mcp_servers: config.get(CODEX_MCP_SERVERS_KEY).cloned(),
            hooks: config.get(CODEX_HOOKS_KEY).cloned(),
        },
    ))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

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

    #[cfg(unix)]
    const FAKE_PLUGIN_BODY: &str = r#"config="$HOME/.codex/config.toml"
peer=''
mcp=''
hooks=''
if [ -f "$config" ]; then
  if /bin/grep -q 'plugins.other' "$config"; then
    peer='[plugins.other]
enabled = true
'
  fi
  if /bin/grep -q 'mcp_servers.tracedecay' "$config"; then
    mcp='[mcp_servers.tracedecay]
command = "/bin/tracedecay"
'
  fi
  if /bin/grep -q '^\[hooks' "$config"; then
    hooks='[hooks.state."keep"]
trusted_hash = "sha256:keep"
'
  fi
fi
case "$1 $2" in
  "plugin add")
    /bin/mkdir -p "$HOME/.codex"
    printf '%s[plugins."tracedecay@personal"]\nenabled = true\n%s%s' "$peer" "$mcp" "$hooks" > "$config"
    ;;
  "plugin remove")
    if [ -n "$peer" ] || [ -n "$mcp" ] || [ -n "$hooks" ]; then
      printf '%s%s%s' "$peer" "$mcp" "$hooks" > "$config"
    else
      /bin/rm -f "$config"
    fi
    ;;
esac
exit 0"#;

    #[cfg(unix)]
    const FAKE_DROPS_HOOK_TRUST_BODY: &str = r#"/bin/mkdir -p "$HOME/.codex"
printf '%s\n' '[plugins."tracedecay@personal"]' 'enabled = true' > "$HOME/.codex/config.toml"
exit 0"#;

    #[cfg(unix)]
    fn shell_single_quote(value: &str) -> String {
        format!("'{}'", value.replace('\'', r"'\''"))
    }

    #[cfg(unix)]
    fn recorded_invocations(log: &Path) -> Vec<String> {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[cfg(unix)]
    fn plugin_enabled(config: &toml::Table, selector: &str) -> Option<bool> {
        config
            .get(CODEX_PLUGINS_KEY)?
            .as_table()?
            .get(selector)?
            .get("enabled")?
            .as_bool()
    }

    #[cfg(unix)]
    const EXPECTED_ADD_INVOCATION: &str = "plugin add tracedecay@personal --json";

    #[cfg(unix)]
    #[test]
    fn activation_drives_codexs_own_plugin_add() {
        let home = tempfile::tempdir().unwrap();
        let bin_dir = tempfile::tempdir().unwrap();
        let log = bin_dir.path().join("invocations.log");
        let codex_cli = bin_dir.path().join("codex");
        fake_codex_cli(&codex_cli, &log, FAKE_PLUGIN_BODY);

        codex_plugin_add_with(&codex_cli, home.path(), "personal")
            .expect("a clean host CLI run is a completed registration");

        assert_eq!(
            recorded_invocations(&log),
            vec![EXPECTED_ADD_INVOCATION.to_string()],
            "activation must add the plugin through Codex's own registry"
        );
        let added: toml::Table =
            toml::from_str(&std::fs::read_to_string(codex_config_path(home.path())).unwrap())
                .unwrap();
        assert_eq!(plugin_enabled(&added, "tracedecay@personal"), Some(true));
    }

    #[cfg(unix)]
    #[test]
    fn removal_drives_codexs_own_plugin_remove_and_reverses_the_registration() {
        let home = tempfile::tempdir().unwrap();
        let bin_dir = tempfile::tempdir().unwrap();
        let log = bin_dir.path().join("invocations.log");
        let codex_cli = bin_dir.path().join("codex");
        fake_codex_cli(&codex_cli, &log, FAKE_PLUGIN_BODY);

        codex_plugin_add_with(&codex_cli, home.path(), "personal").unwrap();
        codex_plugin_remove_with(&codex_cli, home.path(), "personal")
            .expect("a clean host CLI run is a completed removal");

        assert_eq!(
            recorded_invocations(&log),
            vec![
                EXPECTED_ADD_INVOCATION.to_string(),
                "plugin remove tracedecay@personal --json".to_string(),
            ]
        );
        assert!(
            !codex_config_path(home.path()).exists(),
            "removal must fully reverse installation when no peer state remains"
        );
    }

    #[cfg(unix)]
    #[test]
    fn add_and_remove_preserve_peer_plugin_mcp_and_hook_trust() {
        let home = tempfile::tempdir().unwrap();
        let bin_dir = tempfile::tempdir().unwrap();
        let log = bin_dir.path().join("invocations.log");
        let codex_cli = bin_dir.path().join("codex");
        fake_codex_cli(&codex_cli, &log, FAKE_PLUGIN_BODY);
        let config_path = codex_config_path(home.path());
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(
            &config_path,
            concat!(
                "[plugins.other]\nenabled = true\n\n",
                "[mcp_servers.tracedecay]\ncommand = \"/bin/tracedecay\"\n\n",
                "[hooks.state.\"keep\"]\ntrusted_hash = \"sha256:keep\"\n",
            ),
        )
        .unwrap();

        codex_plugin_add_with(&codex_cli, home.path(), "personal")
            .expect("host add must update tracedecay while preserving peers");
        let added: toml::Table =
            toml::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
        assert_eq!(plugin_enabled(&added, "other"), Some(true));
        assert_eq!(plugin_enabled(&added, "tracedecay@personal"), Some(true));
        assert_eq!(
            added
                .get("mcp_servers")
                .and_then(|servers| servers.get("tracedecay"))
                .and_then(|server| server.get("command"))
                .and_then(toml::Value::as_str),
            Some("/bin/tracedecay")
        );
        assert_eq!(
            added
                .get("hooks")
                .and_then(|hooks| hooks.get("state"))
                .and_then(|state| state.get("keep"))
                .and_then(|record| record.get("trusted_hash"))
                .and_then(toml::Value::as_str),
            Some("sha256:keep")
        );

        codex_plugin_remove_with(&codex_cli, home.path(), "personal")
            .expect("host remove must preserve peers while dropping tracedecay");
        let removed: toml::Table =
            toml::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
        assert_eq!(plugin_enabled(&removed, "other"), Some(true));
        assert_eq!(plugin_enabled(&removed, "tracedecay@personal"), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_plugin_command_that_drops_hook_trust_is_refused() {
        let home = tempfile::tempdir().unwrap();
        let bin_dir = tempfile::tempdir().unwrap();
        let log = bin_dir.path().join("invocations.log");
        let codex_cli = bin_dir.path().join("codex");
        fake_codex_cli(&codex_cli, &log, FAKE_DROPS_HOOK_TRUST_BODY);
        let config_path = codex_config_path(home.path());
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(
            &config_path,
            "[hooks.state.\"keep\"]\ntrusted_hash = \"sha256:keep\"\n",
        )
        .unwrap();

        let error = codex_plugin_add_with(&codex_cli, home.path(), "personal")
            .expect_err("a command that drops hook trust must not be accepted");
        let TraceDecayError::Config { message } = error else {
            panic!("a rejected host effect must surface as a config error");
        };
        assert!(
            message.contains("hook trust") && message.contains("unaccepted"),
            "the refusal must name the Codex-owned state it protected: {message}"
        );
    }

    #[test]
    fn a_missing_codex_binary_refuses_instead_of_editing_host_owned_state() {
        let home = tempfile::tempdir().unwrap();
        let config_path = codex_config_path(home.path());
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        let operator_owned = concat!(
            "[plugins.\"someone-else@personal\"]\nenabled = true\n\n",
            "[hooks.state.\"keep\"]\ntrusted_hash = \"sha256:abc\"\n",
        );
        std::fs::write(&config_path, operator_owned).unwrap();

        let error = crate::agents::host_cli::require_host_cli(
            "codex-definitely-absent",
            CODEX_PLUGIN_CLI_LIFECYCLE,
        )
        .expect_err("an absent host binary is a hard requirement failure");
        let TraceDecayError::Config { message } = error else {
            panic!("host CLI absence must surface as a config error");
        };
        assert!(
            message.contains("binary required for codex plugin lifecycle"),
            "the refusal must name the binary and what it was needed for: {message}"
        );
        assert_eq!(
            std::fs::read_to_string(&config_path).unwrap(),
            operator_owned
        );
    }
}
