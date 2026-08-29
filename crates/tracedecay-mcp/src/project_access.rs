/// Tools whose schema advertises a registered-project reader selector.
///
/// Kept here so catalog assembly does not import the root dispatch-binding
/// table. The root binding table remains the authority for dispatch groups;
/// `mcp/tools/binding.rs` tests that this list matches
/// `RegisteredProjectAccess::Reader` rows.
pub fn registered_project_reader_tool_names() -> Vec<&'static str> {
    REGISTERED_PROJECT_READER_TOOL_NAMES.to_vec()
}

const REGISTERED_PROJECT_READER_TOOL_NAMES: &[&str] = &[
    "tracedecay_grep",
    "tracedecay_retrieve",
    "tracedecay_context",
    "tracedecay_callers",
    "tracedecay_callees",
    "tracedecay_impact",
    "tracedecay_node",
    "tracedecay_implementations",
    "tracedecay_callers_for",
    "tracedecay_find_exact_symbol",
    "tracedecay_by_qualified_name",
    "tracedecay_signature",
    "tracedecay_impls",
    "tracedecay_derives",
    "tracedecay_files",
    "tracedecay_type_hierarchy",
    "tracedecay_body",
    "tracedecay_read",
    "tracedecay_outline",
    "tracedecay_signature_search",
    "tracedecay_call_chain",
    "tracedecay_file_dependents",
];
