//! Behavioral discovery and help checks for the shell MCP-tool surface.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::process::Command;

use crate::common::tracedecay_command_with_home;
use tempfile::TempDir;
use tracedecay_mcp::{get_tool_definitions, render_tool_cli_help};

fn isolated_tracedecay_command(home: &TempDir) -> Command {
    let mut command = tracedecay_command_with_home(home.path());
    command.current_dir(home.path());
    command
}

fn short_name(full: &str) -> &str {
    full.strip_prefix("tracedecay_").unwrap_or(full)
}

#[test]
fn every_mcp_tool_is_listed_by_the_cli_discovery_command() {
    let home = TempDir::new().expect("create isolated TraceDecay home");
    let output = isolated_tracedecay_command(&home)
        .arg("tool")
        .output()
        .expect("run `tracedecay tool`");
    assert!(
        output.status.success(),
        "`tracedecay tool` should list tools without needing a project:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let listing = String::from_utf8_lossy(&output.stdout);
    let definitions = get_tool_definitions().expect("tool definitions");
    // The spawned CLI advertises its own registered build version
    // (`<release>+<full sha>[.dirty]`), whose commit this fixture-registered
    // test process cannot know; the count and release version stay exact.
    assert!(
        listing.starts_with(&format!(
            "Available tools ({}; TraceDecay {}",
            definitions.len(),
            tracedecay::version::PACKAGE_VERSION
        )),
        "the CLI catalog must expose its exact count and version so agents can detect a stale MCP"
    );
    let expected = definitions
        .iter()
        .map(|definition| short_name(&definition.name).to_string())
        .collect::<BTreeSet<_>>();
    let listed = listing
        .lines()
        .filter_map(|line| line.strip_prefix("  "))
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| {
            name.chars()
                .all(|character| character.is_ascii_lowercase() || character == '_')
        })
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        listed, expected,
        "the local CLI and MCP must expose the exact same generated tool catalog"
    );
}

#[test]
fn every_mcp_tool_renders_its_own_cli_help() {
    for def in get_tool_definitions().expect("tool definitions") {
        let short = short_name(&def.name);
        let stdout = render_tool_cli_help(&def);
        assert!(
            stdout.contains(&format!("tracedecay tool {short}")),
            "`tracedecay tool {short} --help` should print the tool's own help, got:\n{stdout}"
        );
    }
}

/// One real `tracedecay tool <name> --help` invocation, asserting the binary
/// prints exactly what `render_tool_cli_help` renders. Tool-name resolution
/// and help dispatch are shared across tools, so a single spawn keeps the CLI
/// wiring covered end-to-end without paying one process per tool.
#[test]
fn tool_cli_help_matches_rendered_help_end_to_end() {
    let home = TempDir::new().expect("create isolated TraceDecay home");
    let def = get_tool_definitions()
        .expect("tool definitions")
        .into_iter()
        .next()
        .expect("at least one MCP tool definition");
    let short = short_name(&def.name);
    let output = isolated_tracedecay_command(&home)
        .args(["tool", short, "--help"])
        .output()
        .unwrap_or_else(|e| panic!("run `tracedecay tool {short} --help`: {e}"));
    assert!(
        output.status.success(),
        "`tracedecay tool {short} --help` must succeed so tools stay invocable \
         without an MCP client:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        render_tool_cli_help(&def),
        "CLI help output should be exactly the rendered help"
    );
}
