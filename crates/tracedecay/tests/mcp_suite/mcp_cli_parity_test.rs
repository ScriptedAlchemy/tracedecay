//! CLI ↔ MCP tool-surface parity.
//!
//! Every tool advertised by the MCP registry is rendered by hosts through its
//! description, so the description invariants the host cares about are
//! asserted here: non-empty and within a sane length cap.

use tracedecay_mcp::get_tool_definitions;

/// Host cap on tool description length. Generous — it exists to catch runaway
/// description growth, not to force terseness.
const MAX_DESCRIPTION_CHARS: usize = 8192;

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
