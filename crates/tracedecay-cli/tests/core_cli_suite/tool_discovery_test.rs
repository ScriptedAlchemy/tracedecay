//! Behavioral discovery and help checks for the shell MCP-tool surface.
//!
//! Both sides of every comparison come from this test run: the catalog this
//! test process links and the `tracedecay` binary Cargo built for the same run
//! with the same feature resolution (`CARGO_BIN_EXE_tracedecay`), never a
//! `TRACEDECAY_TEST_BIN` override, a sibling artifact from an older build, or
//! an installed binary on `PATH`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::process::Command;

use crate::common::apply_tracedecay_home_env;
use tempfile::TempDir;
use tracedecay_mcp::{get_tool_definitions, render_tool_cli_help};
use tracedecay_runtime_core::ast_grep::{AST_GREP_BIN_ENV, ast_grep_command};

/// The catalog drops `ast_grep_rewrite` when ast-grep is unavailable, and the
/// child's hermetic `PATH` cannot see an ast-grep the test process found on
/// the operator's `PATH`. Handing the child the executable this process
/// resolved keeps both catalogs probing the same host.
fn isolated_tracedecay_command(home: &TempDir) -> Command {
    let mut command = Command::new(crate::tracedecay_exe());
    apply_tracedecay_home_env(&mut command, home.path());
    command
        .current_dir(home.path())
        .env(AST_GREP_BIN_ENV, ast_grep_command().get_program());
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
            tracedecay_project::version::PACKAGE_VERSION
        )),
        "the CLI catalog must expose its exact count and version so agents can detect a stale MCP, got: {}",
        listing.lines().next().unwrap_or_default()
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
