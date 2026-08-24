//! Conformance between the shipped Cursor plugin bundle and the root MCP
//! tool catalog.
//!
//! These tests live in the root crate because `get_tool_definitions()` is
//! the real tool authority the bundle steers agents toward; the extracted
//! `tracedecay-agent-hosts` crate only sees the catalog through a
//! runtime-registered port that the root crate wires at startup, so the
//! assertions can never hold inside the leaf crate's own test binary.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::{ToolDefinition, get_tool_definitions};
use serde_json::Value;
use std::collections::BTreeSet;
use tracedecay_agent_hosts::agents::plugin_bundle;

/// The Cursor plugin deploy set from the shared bundle authority.
fn embedded_plugin_files() -> Vec<(&'static str, &'static str)> {
    plugin_bundle::cursor_files()
}

/// Every `tracedecay_*` token mentioned anywhere in the embedded plugin
/// bundle (skills, rules, agents, commands, README).
fn embedded_plugin_tool_mentions() -> BTreeSet<String> {
    let mut mentions = BTreeSet::new();
    for (_, contents) in embedded_plugin_files() {
        let bytes = contents.as_bytes();
        let mut search_from = 0;
        while let Some(found) = contents[search_from..].find("tracedecay_") {
            let start = search_from + found;
            let mut end = start + "tracedecay_".len();
            while end < bytes.len()
                && (bytes[end].is_ascii_lowercase()
                    || bytes[end].is_ascii_digit()
                    || bytes[end] == b'_')
            {
                end += 1;
            }
            let token = contents[start..end].trim_end_matches('_');
            if token.len() > "tracedecay_".len() {
                mentions.insert(token.to_string());
            }
            search_from = end;
        }
    }
    mentions
}

/// The full registered tool-name set, independent of host capabilities
/// (`tracedecay_ast_grep_rewrite` is filtered from `get_tool_definitions`
/// when the external `ast-grep` binary is absent, but it is still a real
/// tool the bundle legitimately references).
fn registered_tool_names() -> BTreeSet<String> {
    let mut names: BTreeSet<String> = get_tool_definitions()
        .expect("tool definitions")
        .into_iter()
        .map(|definition| definition.name)
        .collect();
    names.insert("tracedecay_ast_grep_rewrite".to_string());
    names
}

/// Whether a tool definition advertises `readOnlyHint: true`.
fn tool_is_read_only(definition: &ToolDefinition) -> bool {
    definition
        .annotations
        .as_ref()
        .and_then(|annotations| annotations.get("readOnlyHint"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Guards against the plugin steering agents toward tools that do not
/// exist: every `tracedecay_*` name mentioned in the bundle must be a
/// registered MCP tool (or an explicitly allow-listed non-tool marker).
#[test]
fn plugin_tool_mentions_resolve_to_registered_tools() {
    // `tracedecay_metrics` is the savings-report line prefix in tool
    // output, not a tool name.
    const NON_TOOL_MENTIONS: &[&str] = &["tracedecay_metrics"];
    let known = registered_tool_names();
    let unknown: Vec<String> = embedded_plugin_tool_mentions()
        .into_iter()
        .filter(|mention| {
            !known.contains(mention) && !NON_TOOL_MENTIONS.contains(&mention.as_str())
        })
        .collect();
    assert!(
        unknown.is_empty(),
        "cursor-plugin mentions tool names missing from get_tool_definitions(): {unknown:?}"
    );
}

/// Guards against shipping tools no skill/rule/command ever points an
/// agent at (the audit found whole tool families with zero usage because
/// nothing in the bundle referenced them). New tools must either be
/// referenced somewhere under cursor-plugin/ or consciously allow-listed
/// here with a reason.
#[test]
fn registered_tools_are_referenced_by_the_plugin_bundle() {
    // Currently every registered tool is referenced by the bundle. Add a
    // name here only with a written reason for shipping it unsteered.
    const TOOLS_WITHOUT_PLUGIN_REFERENCE: &[&str] = &[];
    let mentions = embedded_plugin_tool_mentions();
    let missing: Vec<String> = registered_tool_names()
        .into_iter()
        .filter(|name| {
            !mentions.contains(name) && !TOOLS_WITHOUT_PLUGIN_REFERENCE.contains(&name.as_str())
        })
        .collect();
    assert!(
        missing.is_empty(),
        "tools registered in get_tool_definitions() but referenced nowhere under \
         cursor-plugin/ (reference them in a skill or allow-list them): {missing:?}"
    );
}

/// The Auto-review allowlist documented in the plugin README must stay in
/// lockstep with the tools' `readOnlyHint` annotations: every read-only
/// tool is listed (so it skips the classifier) and no mutating tool is.
#[test]
fn readme_mcp_allowlist_matches_read_only_tools() {
    let files = embedded_plugin_files();
    let readme = files
        .iter()
        .find(|&&(relative, _)| relative == "README.md")
        .map(|&(_, contents)| contents)
        .expect("plugin README must be embedded");

    let mut listed: Vec<String> = readme
        .lines()
        .filter_map(|line| {
            let entry = line.trim().trim_end_matches(',').trim_matches('"');
            entry
                .strip_prefix("tracedecay:")
                .filter(|tool| tool.starts_with("tracedecay_"))
                .map(str::to_string)
        })
        .collect();
    listed.sort();
    listed.dedup();

    let mut read_only: Vec<String> = get_tool_definitions()
        .expect("tool definitions")
        .into_iter()
        .filter(tool_is_read_only)
        .map(|definition| definition.name)
        .collect();
    read_only.sort();

    assert_eq!(
        listed, read_only,
        "the README mcpAllowlist snippet must list exactly the readOnlyHint=true tools"
    );
}
