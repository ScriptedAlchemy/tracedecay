//! Coverage checks for the shell MCP-tool surface and bundled skill guidance.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::common::tracedecay_command_with_home;
use tempfile::TempDir;
use tracedecay::mcp::tools::{get_tool_definitions, render_tool_cli_help};

/// MCP tools intentionally exempt from bundled-skill coverage.
/// Keep empty unless a tool is truly internal.
const SKILL_COVERAGE_EXCEPTIONS: &[&str] = &[];

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
    assert!(
        listing.starts_with(&format!(
            "Available tools ({}; TraceDecay {})",
            definitions.len(),
            tracedecay::version::build_version()
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

/// True when `haystack` mentions `tool_name` as a standalone identifier
/// (not as a prefix of a longer tool name such as `tracedecay_lcm_expand`
/// inside `tracedecay_lcm_expand_query`).
fn mentions_tool(haystack: &str, tool_name: &str) -> bool {
    let mut rest = haystack;
    while let Some(pos) = rest.find(tool_name) {
        let after = &rest[pos + tool_name.len()..];
        let boundary = after
            .chars()
            .next()
            .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_'));
        if boundary {
            return true;
        }
        rest = after;
    }
    false
}

/// Collects every flat `*.md` command body under a commands directory.
fn command_bodies(commands_root: &Path) -> Vec<String> {
    std::fs::read_dir(commands_root)
        .unwrap_or_else(|e| panic!("read {}: {e}", commands_root.display()))
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("md"))
        .map(|path| std::fs::read_to_string(&path).expect("read command body"))
        .collect()
}

/// Collects every `SKILL.md` body under the given skills-root directories.
fn skill_bodies(skills_roots: &[PathBuf]) -> Vec<String> {
    let mut bodies = Vec::new();
    for skills_root in skills_roots {
        for entry in std::fs::read_dir(skills_root)
            .unwrap_or_else(|e| panic!("read {}: {e}", skills_root.display()))
        {
            let skill_md = entry.expect("skill dir entry").path().join("SKILL.md");
            if !skill_md.is_file() {
                continue;
            }
            let body = std::fs::read_to_string(&skill_md)
                .unwrap_or_else(|e| panic!("read {}: {e}", skill_md.display()));
            bodies.push(body);
        }
    }
    assert!(!bodies.is_empty(), "no skills under {skills_roots:?}");
    bodies
}

#[test]
fn every_mcp_tool_is_taught_by_at_least_one_bundled_skill() {
    let plugin = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin");
    // Codex/Claude deploy the 30 canonical skills under plugin/skills. Cursor
    // deploys the 17 shared model-invocable skills plus the 13 workflow slugs
    // as native commands (`overlays/cursor/commands`). Both host views must
    // teach every MCP tool. `plugin/skills` alone (canonical, 30) is a superset
    // of the shared 17 plus the canonical dispatcher bodies, so it covers the
    // Codex/Claude view; the Cursor view is the 17 shared skills plus the 13
    // command bodies (which carry the workflow tool mentions Cursor ships).
    let codex_claude_bodies = skill_bodies(&[plugin.join("skills")]);
    let mut cursor_bodies: Vec<String> = std::fs::read_dir(plugin.join("skills"))
        .expect("read plugin/skills")
        .flatten()
        .filter(|entry| {
            !entry
                .file_name()
                .to_string_lossy()
                .starts_with("tracedecay-")
        })
        .map(|entry| entry.path().join("SKILL.md"))
        .filter(|path| path.is_file())
        .map(|path| std::fs::read_to_string(&path).expect("read shared skill"))
        .collect();
    cursor_bodies.extend(command_bodies(&plugin.join("overlays/cursor/commands")));

    let host_views: &[(&str, Vec<String>)] = &[
        ("codex/claude (canonical)", codex_claude_bodies),
        ("cursor (shared skills + commands)", cursor_bodies),
    ];
    for (view, bodies) in host_views {
        let mut uncovered: Vec<String> = Vec::new();
        for def in get_tool_definitions().expect("tool definitions") {
            if SKILL_COVERAGE_EXCEPTIONS.contains(&def.name.as_str()) {
                continue;
            }
            let covered = bodies.iter().any(|body| mentions_tool(body, &def.name));
            if !covered {
                uncovered.push(def.name);
            }
        }
        assert!(
            uncovered.is_empty(),
            "MCP tools not referenced by any skill in the {view} view — extend an \
             existing skill or add one so agents can discover them (or, for \
             genuinely internal tools, document them in SKILL_COVERAGE_EXCEPTIONS): \
             {uncovered:?}"
        );
    }
}

#[test]
fn skill_coverage_exceptions_reference_real_tools() {
    let known: Vec<String> = get_tool_definitions()
        .expect("tool definitions")
        .into_iter()
        .map(|def| def.name)
        .collect();
    for exception in SKILL_COVERAGE_EXCEPTIONS {
        assert!(
            known.iter().any(|name| name == exception),
            "SKILL_COVERAGE_EXCEPTIONS entry `{exception}` does not match any \
             registered MCP tool; remove or fix it"
        );
    }
}
