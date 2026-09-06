//! Protocol tests for the compiled host-CLI fixture.
//!
//! These tests exercise the helper as a real process. They must not depend on
//! an ambient Kiro or Codex install, and the provisioned file must be a native
//! executable rather than a renamed script.

#[path = "../build-support/provision_host_cli_fixture.rs"]
mod provision_host_cli_fixture;

use std::fs;
use std::path::Path;
use std::process::Command;

use provision_host_cli_fixture::{
    compiled_host_cli_fixture, install_compiled_host_cli_fixture, looks_like_native_executable,
};
use tempfile::TempDir;

fn run_fixture(bin: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin)
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .output()
        .unwrap_or_else(|error| panic!("failed to spawn host-CLI fixture: {error}"))
}

fn recorded_invocations(home: &Path) -> Vec<String> {
    fs::read_to_string(home.join(".tracedecay-host-cli-fixture/invocations.log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn provisioned_host_cli_fixture_is_a_native_executable() {
    let bytes = fs::read(compiled_host_cli_fixture()).unwrap();
    assert!(
        looks_like_native_executable(&bytes),
        "host-CLI fixture must be a compiled executable, not a script; first bytes: {:?}",
        &bytes[..bytes.len().min(8)]
    );
}

#[test]
fn kiro_fixture_records_install_and_list_arguments() {
    let home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    let bin = install_compiled_host_cli_fixture(bin_dir.path(), "kiro-cli");
    assert!(looks_like_native_executable(&fs::read(&bin).unwrap()));

    let add = run_fixture(
        &bin,
        home.path(),
        &[
            "mcp",
            "add",
            "--name",
            "tracedecay",
            "--command",
            "/usr/local/bin/tracedecay",
            "--args",
            "serve",
            "--scope",
            "global",
            "--force",
        ],
    );
    assert!(
        add.status.success(),
        "kiro mcp add should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&add.stdout),
        String::from_utf8_lossy(&add.stderr)
    );
    let registered: serde_json::Value =
        serde_json::from_slice(&fs::read(home.path().join(".kiro/settings/mcp.json")).unwrap())
            .unwrap();
    assert_eq!(
        registered["mcpServers"]["tracedecay"]["command"],
        "/usr/local/bin/tracedecay"
    );

    let list = run_fixture(&bin, home.path(), &["mcp", "list"]);
    assert!(
        list.status.success(),
        "kiro mcp list should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&list.stdout),
        String::from_utf8_lossy(&list.stderr)
    );
    let listed: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(
        listed["mcpServers"]["tracedecay"]["command"],
        "/usr/local/bin/tracedecay"
    );
    assert_eq!(
        recorded_invocations(home.path()),
        [
            "mcp add --name tracedecay --command /usr/local/bin/tracedecay --args serve --scope global --force",
            "mcp list"
        ]
    );
}

#[test]
fn kiro_fixture_conflict_marker_prints_typed_conflict_output() {
    let home = TempDir::new().unwrap();
    fs::create_dir_all(home.path().join(".tracedecay-host-cli-fixture")).unwrap();
    fs::write(
        home.path().join(".tracedecay-host-cli-fixture/conflict"),
        b"",
    )
    .unwrap();
    let bin_dir = TempDir::new().unwrap();
    let bin = install_compiled_host_cli_fixture(bin_dir.path(), "kiro-cli");

    let output = run_fixture(&bin, home.path(), &["mcp", "list"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("\"error\":\"conflict\""),
        "conflict marker must print the conflict payload\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn kiro_fixture_malformed_marker_writes_invalid_registry_json() {
    let home = TempDir::new().unwrap();
    fs::create_dir_all(home.path().join(".tracedecay-host-cli-fixture")).unwrap();
    fs::write(
        home.path().join(".tracedecay-host-cli-fixture/malformed"),
        b"",
    )
    .unwrap();
    let bin_dir = TempDir::new().unwrap();
    let bin = install_compiled_host_cli_fixture(bin_dir.path(), "kiro-cli");

    let output = run_fixture(
        &bin,
        home.path(),
        &[
            "mcp",
            "add",
            "--name",
            "tracedecay",
            "--command",
            "/usr/local/bin/tracedecay",
            "--args",
            "serve",
            "--scope",
            "global",
            "--force",
        ],
    );
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "not-a-json-document"
    );
    let written = fs::read_to_string(home.path().join(".kiro/settings/mcp.json")).unwrap();
    assert!(
        serde_json::from_str::<serde_json::Value>(&written).is_err(),
        "malformed marker must write invalid registry JSON: {written}"
    );
}

#[test]
fn codex_fixture_installs_lists_and_records_plugin_arguments() {
    let home = TempDir::new().unwrap();
    let source = home.path().join(".codex/plugins/tracedecay/.codex-plugin");
    fs::create_dir_all(&source).unwrap();
    fs::write(
        source.join("plugin.json"),
        format!(r#"{{"version":"{}"}}"#, env!("CARGO_PKG_VERSION")),
    )
    .unwrap();
    let bin_dir = TempDir::new().unwrap();
    let bin = install_compiled_host_cli_fixture(bin_dir.path(), "codex");

    let add = run_fixture(
        &bin,
        home.path(),
        &["plugin", "add", "tracedecay@personal", "--json"],
    );
    assert!(
        add.status.success(),
        "codex plugin add should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&add.stdout),
        String::from_utf8_lossy(&add.stderr)
    );
    let payload: serde_json::Value = serde_json::from_slice(&add.stdout).unwrap();
    assert_eq!(payload["pluginId"], "tracedecay@personal");
    assert!(
        home.path()
            .join(".codex/plugins/cache/personal/tracedecay")
            .join(env!("CARGO_PKG_VERSION"))
            .join(".codex-plugin/plugin.json")
            .is_file(),
        "plugin add must copy the staged source into the versioned cache"
    );

    let list = run_fixture(&bin, home.path(), &["plugin", "list"]);
    assert!(list.status.success());
    let listed: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(listed["plugins"][0]["id"], "tracedecay@personal");
    assert_eq!(
        recorded_invocations(home.path()),
        ["plugin add tracedecay@personal --json", "plugin list"]
    );
}
