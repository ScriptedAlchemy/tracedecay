/// Tools whose `project_selector` dispatches to the selected registered
/// project's server, so reads and writes both use that project's store.
///
/// This list is the sole Reader authority. Root `MCP_TOOL_BINDINGS` derives
/// `RegisteredProjectAccess::Reader` from these names at construction.
pub fn registered_project_reader_tool_names() -> Vec<&'static str> {
    REGISTERED_PROJECT_READER_TOOL_NAMES.to_vec()
}

const REGISTERED_PROJECT_READER_TOOL_NAMES: &[&str] = &[
    "tracedecay_fact_store_search",
    "tracedecay_fact_store_probe",
    "tracedecay_fact_store_related",
    "tracedecay_fact_store_reason",
    "tracedecay_fact_store_contradict",
    "tracedecay_fact_store_get",
    "tracedecay_fact_store_list",
    "tracedecay_fact_store_add",
    "tracedecay_fact_store_update",
    "tracedecay_fact_store_remove",
    "tracedecay_fact_store_supersede",
    "tracedecay_fact_feedback",
    "tracedecay_memory_status",
    "tracedecay_message_search",
    "tracedecay_grep",
    "tracedecay_retrieve",
    "tracedecay_context",
    "tracedecay_callers",
    "tracedecay_callees",
    "tracedecay_impact",
    "tracedecay_node",
    "tracedecay_implementations",
    "tracedecay_find_exact_symbol",
    "tracedecay_by_qualified_name",
    "tracedecay_signature",
    "tracedecay_derives",
    "tracedecay_files",
    "tracedecay_type_hierarchy",
    "tracedecay_signature_search",
    "tracedecay_call_chain",
    "tracedecay_file_dependents",
    "tracedecay_analytics",
];
