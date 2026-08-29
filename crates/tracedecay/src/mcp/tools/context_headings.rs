//! Section headings of the graph context tool's Markdown output.
//!
//! These are the rendering contract shared by the context assemblers
//! (`handlers::graph`) and the response renderer (`render`), which uses the
//! priority order to decide which sections survive truncation.

pub(crate) const CONTEXT_MEMORY_MATCHES_HEADING: &str = "### Memory Matches";
pub(crate) const CONTEXT_MEMORY_FEEDBACK_HINT: &str = "Rate what you use: call tracedecay_fact_feedback with a fact_id above — action=helpful if a fact steered you right, action=unhelpful if it was wrong or misleading. Flagging a bad fact matters as much as confirming a good one; trust is earned only from this feedback, so rate the ones you actually used.";
pub(crate) const CONTEXT_ENTRY_POINTS_HEADING: &str = "### Entry Points";
pub(crate) const CONTEXT_RELATED_SYMBOLS_HEADING: &str = "### Related Symbols";
pub(crate) const CONTEXT_CODE_HEADING: &str = "### Code";
pub(crate) const CONTEXT_INDEX_COVERAGE_HINT_HEADING: &str = "### Index Coverage Hint";
pub(crate) const CONTEXT_EXTENSION_POINTS_HEADING: &str = "### Extension Points";
pub(crate) const CONTEXT_TEST_COVERAGE_HEADING: &str = "### Test Coverage";
pub(crate) const CONTEXT_SEEN_NODE_IDS_LABEL: &str = "seen_node_ids:";
pub(crate) const CONTEXT_PRIORITY_HEADINGS: &[&str] = &[
    CONTEXT_MEMORY_MATCHES_HEADING,
    CONTEXT_ENTRY_POINTS_HEADING,
    CONTEXT_RELATED_SYMBOLS_HEADING,
    CONTEXT_INDEX_COVERAGE_HINT_HEADING,
    CONTEXT_EXTENSION_POINTS_HEADING,
    CONTEXT_TEST_COVERAGE_HEADING,
    CONTEXT_SEEN_NODE_IDS_LABEL,
    CONTEXT_CODE_HEADING,
];
