//! Named context-section headings shared by catalog rendering and the
//! composition-root formatter. One table so truncation and markdown stay
//! aligned.

pub const CODE_CONTEXT_HEADING: &str = "## Code Context";
pub const CONTEXT_MEMORY_MATCHES_HEADING: &str = "### Memory Matches";
pub const CONTEXT_MEMORY_FEEDBACK_HINT: &str = "Rate what you use: call tracedecay_fact_feedback with a fact_id above — action=helpful if a fact steered you right, action=unhelpful if it was wrong or misleading. Flagging a bad fact matters as much as confirming a good one; trust is earned only from this feedback, so rate the ones you actually used.";
pub const CONTEXT_ENTRY_POINTS_HEADING: &str = "### Entry Points";
pub const CONTEXT_RELATED_SYMBOLS_HEADING: &str = "### Related Symbols";
pub const CONTEXT_CODE_HEADING: &str = "### Code";
pub const CONTEXT_INDEX_COVERAGE_HINT_HEADING: &str = "### Index Coverage Hint";
pub const CONTEXT_EXTENSION_POINTS_HEADING: &str = "### Extension Points";
pub const CONTEXT_TEST_COVERAGE_HEADING: &str = "### Test Coverage";
pub const CONTEXT_SEEN_NODE_IDS_LABEL: &str = "seen_node_ids:";

/// Late-priority sections kept when a context response is truncated.
pub const CONTEXT_PRIORITY_HEADINGS: &[&str] = &[
    CONTEXT_MEMORY_MATCHES_HEADING,
    CONTEXT_ENTRY_POINTS_HEADING,
    CONTEXT_RELATED_SYMBOLS_HEADING,
    CONTEXT_INDEX_COVERAGE_HINT_HEADING,
    CONTEXT_EXTENSION_POINTS_HEADING,
    CONTEXT_TEST_COVERAGE_HEADING,
    CONTEXT_SEEN_NODE_IDS_LABEL,
    CONTEXT_CODE_HEADING,
];
