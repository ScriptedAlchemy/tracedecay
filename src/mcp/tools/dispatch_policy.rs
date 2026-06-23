//! MCP tool dispatch policy shared by schema generation and handlers.

pub(super) const REGISTERED_PROJECT_READER_TOOL_NAMES: &[&str] = &[
    "tracedecay_search",
    "tracedecay_context",
    "tracedecay_retrieve",
    "tracedecay_callers",
    "tracedecay_callees",
    "tracedecay_impact",
    "tracedecay_node",
    "tracedecay_files",
    "tracedecay_body",
    "tracedecay_read",
    "tracedecay_outline",
    "tracedecay_signature_search",
    "tracedecay_implementations",
    "tracedecay_callers_for",
    "tracedecay_call_chain",
    "tracedecay_file_dependents",
    "tracedecay_find_exact_symbol",
    "tracedecay_by_qualified_name",
    "tracedecay_signature",
    "tracedecay_impls",
    "tracedecay_derives",
];

const REGISTERED_PROJECT_SELECTOR_ONLY_TOOL_NAMES: &[&str] = &[
    "tracedecay_project_context",
    "tracedecay_fact_store",
    "tracedecay_memory_status",
    "tracedecay_message_search",
];

pub(super) fn tool_accepts_registered_project_selector(tool_name: &str) -> bool {
    tool_dispatches_registered_project_reader(tool_name)
        || REGISTERED_PROJECT_SELECTOR_ONLY_TOOL_NAMES.contains(&tool_name)
}

pub(super) fn tool_dispatches_registered_project_reader(tool_name: &str) -> bool {
    REGISTERED_PROJECT_READER_TOOL_NAMES.contains(&tool_name)
}
