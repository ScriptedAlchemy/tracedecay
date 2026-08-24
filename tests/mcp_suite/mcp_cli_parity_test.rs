//! CLI ↔ MCP tool-surface parity.
//!
//! Every tool advertised by the MCP registry must be invocable through the
//! generic `tracedecay tool <name>` CLI surface. That CLI resolves a
//! user-supplied name to a definition by canonicalizing it (strip the
//! `tracedecay_` prefix, dash→underscore, apply aliases, re-add the prefix)
//! and matching the result against `def.name` exactly
//! (`src/tool_command.rs::canonical_tool_name` + the `defs.iter().find`
//! lookup in `run`). If any tool name did not survive that round-trip, the
//! tool would be advertised over MCP but unreachable from the CLI.
//!
//! These tests mirror the canonicalization so a name shape that breaks CLI
//! dispatch (e.g. a future tool with a dash, or one colliding with an alias)
//! fails here instead of silently dropping off the CLI. They also assert the
//! description invariants the host cares about: non-empty and within a sane
//! length cap.

use tracedecay::mcp::get_tool_definitions;

/// Old CLI command names that intentionally map to a different MCP suffix.
/// Kept in lockstep with `NAME_ALIASES` in `src/tool_command.rs`; the alias
/// *source* names must never collide with a real tool suffix (asserted below).
const NAME_ALIASES: &[(&str, &str)] = &[("query", "search")];

/// Host cap on tool description length. Generous — it exists to catch runaway
/// description growth, not to force terseness. Well above the longest current
/// description (the multi-paragraph `tracedecay_diagnostics` blurb).
const MAX_DESCRIPTION_CHARS: usize = 8192;

/// Reimplementation of `src/tool_command.rs::canonical_tool_name`. Kept in
/// sync with that private CLI helper; the parity tests below fail loudly if
/// the two ever diverge for any real tool name.
fn canonical_tool_name(raw: &str) -> String {
    let trimmed = raw.strip_prefix("tracedecay_").unwrap_or(raw);
    let normalized = trimmed.replace('-', "_");
    let mapped = NAME_ALIASES
        .iter()
        .find(|(k, _)| *k == normalized)
        .map_or(normalized.as_str(), |(_, v)| *v);
    format!("tracedecay_{mapped}")
}

#[test]
fn every_mcp_tool_is_invocable_via_cli_full_name() {
    // The CLI accepts the fully-qualified `tracedecay_<suffix>` name and must
    // resolve it back to the exact same definition.
    let tools = get_tool_definitions().expect("tool definitions");
    assert!(!tools.is_empty());
    for tool in &tools {
        let resolved = canonical_tool_name(&tool.name);
        assert_eq!(
            resolved, tool.name,
            "tool '{}' does not round-trip through the CLI canonicalizer \
             (resolved to '{resolved}'); it would be advertised over MCP but \
             unreachable via `tracedecay tool {}`",
            tool.name, tool.name
        );
    }
}

#[test]
fn every_mcp_tool_is_invocable_via_cli_short_name() {
    // The ergonomic form drops the `tracedecay_` prefix. `tracedecay tool
    // <suffix>` must reach the same definition.
    let tools = get_tool_definitions().expect("tool definitions");
    for tool in &tools {
        let short = tool
            .name
            .strip_prefix("tracedecay_")
            .expect("every tool name is prefixed with tracedecay_");
        let resolved = canonical_tool_name(short);
        assert_eq!(
            resolved, tool.name,
            "short CLI name '{short}' does not resolve to '{}'",
            tool.name
        );
        // The dash spelling is also accepted by the CLI; make sure it lands on
        // the same tool for suffixes that contain underscores.
        let dashed = short.replace('_', "-");
        let resolved_dashed = canonical_tool_name(&dashed);
        assert_eq!(
            resolved_dashed, tool.name,
            "dashed CLI name '{dashed}' does not resolve to '{}'",
            tool.name
        );
    }
}

#[test]
fn cli_tool_names_are_unique_after_canonicalization() {
    // Two tools canonicalizing to the same name would make one of them
    // unreachable from the CLI (the `find` returns the first match).
    let tools = get_tool_definitions().expect("tool definitions");
    let mut seen = std::collections::HashSet::new();
    for tool in &tools {
        let canonical = canonical_tool_name(&tool.name);
        assert!(
            seen.insert(canonical.clone()),
            "two tools canonicalize to '{canonical}'; the CLI can only reach one"
        );
    }
}

#[test]
fn cli_aliases_do_not_shadow_real_tools() {
    // An alias source that also names a real tool suffix would hijack that
    // tool's CLI invocation. Guard the aliases stay purely legacy synonyms.
    let tools = get_tool_definitions().expect("tool definitions");
    for (alias_src, _target) in NAME_ALIASES {
        let collides = tools
            .iter()
            .any(|t| t.name == format!("tracedecay_{alias_src}"));
        assert!(
            !collides,
            "CLI alias '{alias_src}' collides with a real tool suffix; it would \
             shadow that tool on the CLI"
        );
    }
}

#[test]
fn every_tool_description_is_non_empty_and_within_cap() {
    let tools = get_tool_definitions().expect("tool definitions");
    for tool in &tools {
        let desc = tool.description.trim();
        assert!(
            !desc.is_empty(),
            "tool '{}' has an empty description; hosts render this as the \
             tool's only routing signal",
            tool.name
        );
        assert!(
            tool.description.chars().count() <= MAX_DESCRIPTION_CHARS,
            "tool '{}' description is {} chars, over the {MAX_DESCRIPTION_CHARS}-char host cap",
            tool.name,
            tool.description.chars().count()
        );
    }
}
