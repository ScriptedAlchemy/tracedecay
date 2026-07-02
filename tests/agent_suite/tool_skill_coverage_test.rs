//! Reverse coverage contract for the MCP tool surface.
//!
//! The MCP server may one day be optional (or expose only a subset of
//! tools), so the CLI must stay a fully self-sufficient interface. The
//! forward direction — every skill-referenced tool resolving to a real MCP
//! definition — is covered elsewhere; these tests pin the reverse:
//!
//! 1. every advertised MCP tool is invocable via `tracedecay tool <name>`
//!    (end-to-end through the shipped binary), and
//! 2. every advertised MCP tool is taught by at least one bundled skill in
//!    each plugin bundle, so agents can discover when and how to use it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::process::Command;

use tracedecay::mcp::tools::get_tool_definitions;

/// Tools intentionally exempt from the skill-coverage requirement.
///
/// Reserve this for genuinely internal tools (host-lifecycle plumbing an
/// agent should never choose from a skill). Every entry needs a comment
/// explaining why no skill should teach it. Currently every advertised tool
/// is agent-facing and covered.
const SKILL_COVERAGE_EXCEPTIONS: &[&str] = &[];

fn tracedecay_bin() -> &'static str {
    env!("CARGO_BIN_EXE_tracedecay")
}

fn short_name(full: &str) -> &str {
    full.strip_prefix("tracedecay_").unwrap_or(full)
}

#[test]
fn every_mcp_tool_is_listed_by_the_cli_discovery_command() {
    let output = Command::new(tracedecay_bin())
        .arg("tool")
        .current_dir(std::env::temp_dir())
        .output()
        .expect("run `tracedecay tool`");
    assert!(
        output.status.success(),
        "`tracedecay tool` should list tools without needing a project:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let listing = String::from_utf8_lossy(&output.stdout);
    for def in get_tool_definitions() {
        let short = short_name(&def.name);
        assert!(
            listing.contains(short),
            "`tracedecay tool` listing is missing `{short}` — the CLI discovery \
             surface must cover every MCP tool"
        );
    }
}

#[test]
fn every_mcp_tool_is_invocable_via_the_cli() {
    for def in get_tool_definitions() {
        let short = short_name(&def.name);
        let output = Command::new(tracedecay_bin())
            .args(["tool", short, "--help"])
            .current_dir(std::env::temp_dir())
            .output()
            .unwrap_or_else(|e| panic!("run `tracedecay tool {short} --help`: {e}"));
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "`tracedecay tool {short} --help` must succeed so the tool stays \
             invocable without an MCP client:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            stdout.contains(&format!("tracedecay tool {short}")),
            "`tracedecay tool {short} --help` should print the tool's own help, got:\n{stdout}"
        );
    }
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

fn bundle_skill_bodies(bundle_root: &Path) -> Vec<String> {
    let skills_root = bundle_root.join("skills");
    let mut bodies = Vec::new();
    for entry in std::fs::read_dir(&skills_root)
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
    assert!(
        !bodies.is_empty(),
        "no skills under {}",
        skills_root.display()
    );
    bodies
}

#[test]
fn every_mcp_tool_is_taught_by_at_least_one_bundled_skill() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for bundle in ["cursor-plugin", "codex-plugin"] {
        let bodies = bundle_skill_bodies(&repo_root.join(bundle));
        let mut uncovered: Vec<String> = Vec::new();
        for def in get_tool_definitions() {
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
            "MCP tools not referenced by any {bundle} skill — extend an existing \
             skill or add one so agents can discover them (or, for genuinely \
             internal tools, document them in SKILL_COVERAGE_EXCEPTIONS): {uncovered:?}"
        );
    }
}

#[test]
fn skill_coverage_exceptions_reference_real_tools() {
    let known: Vec<String> = get_tool_definitions()
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
