//! Host-CLI-driven GitHub Copilot MCP registry lifecycle.
//!
//! Copilot owns `~/.copilot/mcp-config.json` through `copilot mcp`, so
//! TraceDecay drives that CLI rather than merging the file. These tests stand a
//! fake `copilot` in an isolated HOME (never the operator's real `~/.copilot`),
//! assert the exact argv TraceDecay issues, and assert that an absent binary
//! refuses instead of falling back to config surgery.
//!
//! The fake CLI emulates the registry's own effect (add writes the server
//! entry, remove drops it) so removal can be shown to *reverse* installation
//! rather than merely being spelled correctly, and it preserves a known peer
//! server so the lifecycle's preservation guard is exercised on both add and
//! remove. A second fake deliberately discards the peer, proving the guard
//! refuses instead of accepting the loss.

use super::*;

/// Install a fake `copilot` that appends each invocation's argv to `log` and
/// then performs `body`.
#[cfg(unix)]
fn fake_copilot_cli(bin: &Path, log: &Path, body: &str) {
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

/// Body for a fake `copilot` that emulates the registry's own writes, so a
/// test can observe that TraceDecay's removal really reverses its install.
///
/// The `--` guard is load-bearing: Copilot's non-interactive form puts the
/// server's launch command line *after* a separator, and a registration that
/// forgot it would otherwise pass unnoticed.
#[cfg(unix)]
const FAKE_REGISTRY_BODY: &str = r#"case "$1 $2" in
  "mcp add")
    [ "$4" = "--" ] || { echo 'missing -- separator before the server command' >&2; exit 64; }
    command="$5"
    /bin/mkdir -p "$HOME/.copilot"
    if [ -f "$HOME/.copilot/mcp-config.json" ] && /bin/grep -q '"other"' "$HOME/.copilot/mcp-config.json"; then
      printf '{"mcpServers":{"other":{"command":"other","args":[]},"tracedecay":{"command":"%s","args":["serve"]}}}\n' "$command" > "$HOME/.copilot/mcp-config.json"
    else
      printf '{"mcpServers":{"tracedecay":{"command":"%s","args":["serve"]}}}\n' "$command" > "$HOME/.copilot/mcp-config.json"
    fi
    ;;
  "mcp remove")
    if [ -f "$HOME/.copilot/mcp-config.json" ] && /bin/grep -q '"other"' "$HOME/.copilot/mcp-config.json"; then
      printf '%s\n' '{"mcpServers":{"other":{"command":"other","args":[]}}}' > "$HOME/.copilot/mcp-config.json"
    else
      /bin/rm -f "$HOME/.copilot/mcp-config.json"
    fi
    ;;
esac
exit 0"#;

/// A host command that reports success while discarding the operator's other
/// MCP servers. TraceDecay must not accept that state.
#[cfg(unix)]
const FAKE_PEER_DISCARDING_BODY: &str = r#"case "$1 $2" in
  "mcp add")
    /bin/mkdir -p "$HOME/.copilot"
    printf '%s\n' '{"mcpServers":{"tracedecay":{"command":"/bin/tracedecay","args":["serve"]}}}' > "$HOME/.copilot/mcp-config.json"
    ;;
esac
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
#[test]
fn activation_drives_the_hosts_own_mcp_add_with_the_registered_server_contract() {
    let home = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    let log = bin_dir.path().join("invocations.log");
    let copilot_cli = bin_dir.path().join("copilot");
    fake_copilot_cli(&copilot_cli, &log, FAKE_REGISTRY_BODY);

    copilot_mcp_add_with(&copilot_cli, home.path(), "/bin/tracedecay")
        .expect("a clean host CLI run is a completed registration");

    assert_eq!(
        recorded_invocations(&log),
        vec!["mcp add tracedecay -- /bin/tracedecay serve".to_string()],
        "activation must add the server through Copilot's own registry, naming it and \
         passing the launch command line after the `--` separator"
    );
    assert!(
        copilot_cli_mcp_config_path(home.path()).exists(),
        "the host's own registry write must be what lands the entry"
    );
}

#[cfg(unix)]
#[test]
fn removal_drives_the_hosts_own_mcp_remove_and_reverses_the_registration() {
    let home = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    let log = bin_dir.path().join("invocations.log");
    let copilot_cli = bin_dir.path().join("copilot");
    fake_copilot_cli(&copilot_cli, &log, FAKE_REGISTRY_BODY);
    let mcp_path = copilot_cli_mcp_config_path(home.path());
    assert!(!mcp_path.exists(), "precondition: nothing registered yet");

    copilot_mcp_add_with(&copilot_cli, home.path(), "/bin/tracedecay").unwrap();
    copilot_mcp_remove_with(&copilot_cli, home.path())
        .expect("a clean host CLI run is a completed removal");

    assert_eq!(
        recorded_invocations(&log),
        vec![
            "mcp add tracedecay -- /bin/tracedecay serve".to_string(),
            "mcp remove tracedecay".to_string(),
        ],
        "removal must address the server by the same registry name the add used"
    );
    assert!(
        !mcp_path.exists(),
        "removal must fully reverse installation, leaving no tracedecay entry behind"
    );
}

#[cfg(unix)]
#[test]
fn add_and_remove_preserve_an_operator_owned_peer_server() {
    let home = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    let log = bin_dir.path().join("invocations.log");
    let copilot_cli = bin_dir.path().join("copilot");
    fake_copilot_cli(&copilot_cli, &log, FAKE_REGISTRY_BODY);
    let mcp_path = copilot_cli_mcp_config_path(home.path());
    std::fs::create_dir_all(mcp_path.parent().unwrap()).unwrap();
    std::fs::write(
        &mcp_path,
        br#"{"mcpServers":{"other":{"command":"other","args":[]},"tracedecay":{"command":"/old/tracedecay","args":["serve"]}}}"#,
    )
    .unwrap();

    copilot_mcp_add_with(&copilot_cli, home.path(), "/new/tracedecay")
        .expect("host add must update tracedecay while preserving the peer");
    let added: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&mcp_path).unwrap()).unwrap();
    assert_eq!(added["mcpServers"]["other"]["command"], "other");
    assert_eq!(
        added["mcpServers"]["tracedecay"]["command"],
        "/new/tracedecay"
    );

    copilot_mcp_remove_with(&copilot_cli, home.path())
        .expect("host remove must preserve the peer while dropping tracedecay");
    let removed: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&mcp_path).unwrap()).unwrap();
    assert_eq!(removed["mcpServers"]["other"]["command"], "other");
    assert!(removed["mcpServers"].get("tracedecay").is_none());
}

#[cfg(unix)]
#[test]
fn a_host_command_that_discards_a_peer_server_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    let log = bin_dir.path().join("invocations.log");
    let copilot_cli = bin_dir.path().join("copilot");
    fake_copilot_cli(&copilot_cli, &log, FAKE_PEER_DISCARDING_BODY);
    let mcp_path = copilot_cli_mcp_config_path(home.path());
    std::fs::create_dir_all(mcp_path.parent().unwrap()).unwrap();
    std::fs::write(
        &mcp_path,
        br#"{"mcpServers":{"other":{"command":"other","args":[]}}}"#,
    )
    .unwrap();

    let error = copilot_mcp_add_with(&copilot_cli, home.path(), "/bin/tracedecay")
        .expect_err("losing an operator-owned peer must not be accepted as a registration");

    let TraceDecayError::Config { message } = error else {
        panic!("a peer-preservation refusal must surface as a config error");
    };
    assert!(
        message.contains("changed peer MCP servers")
            && message.contains("mcp add tracedecay")
            && message.contains(&mcp_path.display().to_string()),
        "the refusal must name the invocation and the registry path: {message}"
    );
}

#[cfg(unix)]
#[test]
fn a_failing_copilot_registry_command_reports_the_hosts_own_diagnosis() {
    let home = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    let log = bin_dir.path().join("invocations.log");
    let copilot_cli = bin_dir.path().join("copilot");
    fake_copilot_cli(
        &copilot_cli,
        &log,
        "echo 'mcp server tracedecay is not configured' >&2\nexit 7",
    );

    let error = copilot_mcp_remove_with(&copilot_cli, home.path())
        .expect_err("a non-zero host CLI exit must fail the lifecycle");

    let TraceDecayError::Config { message } = error else {
        panic!("a failed host command must surface as a config error");
    };
    assert!(
        message.contains("mcp server tracedecay is not configured")
            && message.contains("exit code 7"),
        "the host's own stderr and status must reach the operator: {message}"
    );
}

#[test]
fn a_missing_copilot_binary_refuses_instead_of_editing_host_owned_state() {
    let home = tempfile::tempdir().unwrap();
    let mcp_path = copilot_cli_mcp_config_path(home.path());
    std::fs::create_dir_all(mcp_path.parent().unwrap()).unwrap();
    let operator_owned = br#"{"mcpServers":{"someone-elses":{"command":"other"}}}"#;
    std::fs::write(&mcp_path, operator_owned).unwrap();

    let error = crate::agents::host_cli::require_host_cli("copilot-absent", COPILOT_CLI_LIFECYCLE)
        .expect_err("an absent host binary is a hard requirement failure");

    let TraceDecayError::HostCliUnavailable { program, lifecycle } = error else {
        panic!("host CLI absence must surface as a typed requirement");
    };
    assert_eq!(program, "copilot-absent");
    assert_eq!(lifecycle, COPILOT_CLI_LIFECYCLE);
    assert_eq!(
        std::fs::read(&mcp_path).unwrap(),
        operator_owned,
        "a refused lifecycle must not have touched host-owned registry state"
    );
    assert!(
        !config_backup_path(&mcp_path).exists(),
        "a refused lifecycle must not have staged a backup of host-owned registry state"
    );
}

#[test]
fn the_registry_path_is_derived_from_the_admitted_profile_home() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(
        copilot_cli_mcp_config_path(home.path()),
        home.path().join(".copilot/mcp-config.json"),
        "the registry read must follow the same profile the host command is given as HOME"
    );
}

#[test]
fn the_doctor_readback_accepts_exactly_the_cli_launch_arguments() {
    let mut registered = serde_json::Map::new();
    registered.insert(
        "args".to_string(),
        serde_json::to_value(MCP_SERVER_ARGS).unwrap(),
    );
    assert!(
        server_args_are_current(&registered),
        "the arguments the CLI registration passes must be the arguments the doctor accepts"
    );

    let mut stale = serde_json::Map::new();
    stale.insert(
        "args".to_string(),
        serde_json::Value::Array(vec![serde_json::Value::String("not-serve".to_string())]),
    );
    assert!(
        !server_args_are_current(&stale),
        "a server launched with different arguments must not read back as current"
    );
}
