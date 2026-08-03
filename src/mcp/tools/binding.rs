//! Canonical binding between an MCP tool name and how the server treats it.
//!
//! One table answers both questions the dispatcher and the schema layer used
//! to answer from separate name lists: which dispatch group owns a tool, and
//! whether the tool accepts a registered-project selector. Keeping them in one
//! row means a tool cannot gain a dispatch group while silently keeping the
//! wrong project access, which is what two independent lists allowed.
//!
//! `group` is `None` for tools whose group resolves dynamically through the
//! application-surface or retained-surface predicates; those predicates remain
//! the authority for their own tools and are not duplicated here.
//!
//! A tool may hold both a surface predicate and a row here when the classifier
//! deliberately declines the surface for it. `tracedecay_diagnostics` is the
//! one such tool: it is an application-surface operation, but when no daemon
//! invocation executor is attached the classifier defers it to the analysis
//! group, and this row is what the deferred lookup resolves against.

use std::collections::HashMap;
use std::sync::LazyLock;

use tracedecay_tool_catalog::{
    BindingSurface, CancellationContract, CancellationPoint, EffectClass, McpDeadlineContractV1,
    McpDispatchAvailability, McpDispatchCatalogV1, McpDispatchContractInputV1,
    McpDispatchContractV1, McpDispatchUnavailableReason, McpIdempotencyContract,
    McpInverseContract, McpInverseUnavailableReason, McpTerminalState,
};

/// Which dispatch family owns a tool once the surface predicates decline it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum McpToolDispatchGroup {
    ApplicationSurface,
    Graph,
    Info,
    Admin,
    Analysis,
    Git,
    Edit,
    Health,
    RetainedApplication,
    Memory,
    SessionWorkflow,
}

/// How a tool may be pointed at a project other than the active one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RegisteredProjectAccess {
    /// Reads bind to whichever project is active; a selector is rejected.
    ActiveProjectOnly,
    /// Accepts a selector but does not dispatch a registered-project reader.
    SelectorOnly,
    /// Accepts a selector and dispatches against the selected project's store.
    Reader,
}

pub(crate) struct McpToolBinding {
    pub(crate) name: &'static str,
    pub(crate) group: Option<McpToolDispatchGroup>,
    pub(crate) project: RegisteredProjectAccess,
}

#[rustfmt::skip]
pub(crate) const MCP_TOOL_BINDINGS: &[McpToolBinding] = &[
    McpToolBinding { name: "tracedecay_search", group: Some(McpToolDispatchGroup::Graph), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_grep", group: Some(McpToolDispatchGroup::Graph), project: RegisteredProjectAccess::Reader },
    McpToolBinding { name: "tracedecay_ast_grep_search", group: Some(McpToolDispatchGroup::Graph), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_retrieve", group: Some(McpToolDispatchGroup::Graph), project: RegisteredProjectAccess::Reader },
    McpToolBinding { name: "tracedecay_context", group: Some(McpToolDispatchGroup::Graph), project: RegisteredProjectAccess::Reader },
    McpToolBinding { name: "tracedecay_callers", group: Some(McpToolDispatchGroup::Graph), project: RegisteredProjectAccess::Reader },
    McpToolBinding { name: "tracedecay_callees", group: Some(McpToolDispatchGroup::Graph), project: RegisteredProjectAccess::Reader },
    McpToolBinding { name: "tracedecay_impact", group: Some(McpToolDispatchGroup::Graph), project: RegisteredProjectAccess::Reader },
    McpToolBinding { name: "tracedecay_node", group: Some(McpToolDispatchGroup::Graph), project: RegisteredProjectAccess::Reader },
    McpToolBinding { name: "tracedecay_similar", group: Some(McpToolDispatchGroup::Graph), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_rename_preview", group: Some(McpToolDispatchGroup::Graph), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_implementations", group: Some(McpToolDispatchGroup::Graph), project: RegisteredProjectAccess::Reader },
    McpToolBinding { name: "tracedecay_callers_for", group: Some(McpToolDispatchGroup::Graph), project: RegisteredProjectAccess::Reader },
    McpToolBinding { name: "tracedecay_find_exact_symbol", group: Some(McpToolDispatchGroup::Graph), project: RegisteredProjectAccess::Reader },
    McpToolBinding { name: "tracedecay_by_qualified_name", group: Some(McpToolDispatchGroup::Graph), project: RegisteredProjectAccess::Reader },
    McpToolBinding { name: "tracedecay_signature", group: Some(McpToolDispatchGroup::Graph), project: RegisteredProjectAccess::Reader },
    McpToolBinding { name: "tracedecay_impls", group: Some(McpToolDispatchGroup::Graph), project: RegisteredProjectAccess::Reader },
    McpToolBinding { name: "tracedecay_derives", group: Some(McpToolDispatchGroup::Graph), project: RegisteredProjectAccess::Reader },
    McpToolBinding { name: "tracedecay_status", group: Some(McpToolDispatchGroup::Info), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_active_project", group: Some(McpToolDispatchGroup::Info), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_project_list", group: Some(McpToolDispatchGroup::Info), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_project_search", group: Some(McpToolDispatchGroup::Info), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_project_context", group: Some(McpToolDispatchGroup::Info), project: RegisteredProjectAccess::SelectorOnly },
    McpToolBinding { name: "tracedecay_files", group: Some(McpToolDispatchGroup::Info), project: RegisteredProjectAccess::Reader },
    McpToolBinding { name: "tracedecay_admin_sync", group: Some(McpToolDispatchGroup::Info), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_port_status", group: Some(McpToolDispatchGroup::Info), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_port_order", group: Some(McpToolDispatchGroup::Info), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_simplify_scan", group: Some(McpToolDispatchGroup::Info), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_type_hierarchy", group: Some(McpToolDispatchGroup::Info), project: RegisteredProjectAccess::Reader },
    McpToolBinding { name: "tracedecay_body", group: Some(McpToolDispatchGroup::Info), project: RegisteredProjectAccess::Reader },
    McpToolBinding { name: "tracedecay_todos", group: Some(McpToolDispatchGroup::Info), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_read", group: Some(McpToolDispatchGroup::Info), project: RegisteredProjectAccess::Reader },
    McpToolBinding { name: "tracedecay_outline", group: Some(McpToolDispatchGroup::Info), project: RegisteredProjectAccess::Reader },
    McpToolBinding { name: "tracedecay_config", group: Some(McpToolDispatchGroup::Info), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_signature_search", group: Some(McpToolDispatchGroup::Info), project: RegisteredProjectAccess::Reader },
    McpToolBinding { name: "tracedecay_hook_runtime", group: Some(McpToolDispatchGroup::Admin), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_admin_cli", group: Some(McpToolDispatchGroup::Admin), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_admin_project", group: Some(McpToolDispatchGroup::Admin), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_dead_code", group: Some(McpToolDispatchGroup::Analysis), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_circular", group: Some(McpToolDispatchGroup::Analysis), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_hotspots", group: Some(McpToolDispatchGroup::Analysis), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_unused_imports", group: Some(McpToolDispatchGroup::Analysis), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_rank", group: Some(McpToolDispatchGroup::Analysis), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_largest", group: Some(McpToolDispatchGroup::Analysis), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_coupling", group: Some(McpToolDispatchGroup::Analysis), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_inheritance_depth", group: Some(McpToolDispatchGroup::Analysis), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_distribution", group: Some(McpToolDispatchGroup::Analysis), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_recursion", group: Some(McpToolDispatchGroup::Analysis), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_complexity", group: Some(McpToolDispatchGroup::Analysis), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_doc_coverage", group: Some(McpToolDispatchGroup::Analysis), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_god_class", group: Some(McpToolDispatchGroup::Analysis), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_unsafe_patterns", group: Some(McpToolDispatchGroup::Analysis), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_constructors", group: Some(McpToolDispatchGroup::Analysis), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_field_sites", group: Some(McpToolDispatchGroup::Analysis), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_diagnostics", group: Some(McpToolDispatchGroup::Analysis), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_admin_branch_add", group: Some(McpToolDispatchGroup::Git), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_affected", group: Some(McpToolDispatchGroup::Git), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_diff_context", group: Some(McpToolDispatchGroup::Git), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_changelog", group: Some(McpToolDispatchGroup::Git), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_commit_context", group: Some(McpToolDispatchGroup::Git), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_pr_context", group: Some(McpToolDispatchGroup::Git), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_branch_search", group: Some(McpToolDispatchGroup::Git), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_branch_diff", group: Some(McpToolDispatchGroup::Git), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_branch_list", group: Some(McpToolDispatchGroup::Git), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_str_replace", group: Some(McpToolDispatchGroup::Edit), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_multi_str_replace", group: Some(McpToolDispatchGroup::Edit), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_insert_at", group: Some(McpToolDispatchGroup::Edit), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_ast_grep_rewrite", group: Some(McpToolDispatchGroup::Edit), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_replace_symbol", group: Some(McpToolDispatchGroup::Edit), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_insert_at_symbol", group: Some(McpToolDispatchGroup::Edit), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_move_symbol", group: Some(McpToolDispatchGroup::Edit), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_api_migration_plan", group: Some(McpToolDispatchGroup::Edit), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_api_migration_apply", group: Some(McpToolDispatchGroup::Edit), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_source_edit_reconcile", group: Some(McpToolDispatchGroup::Edit), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_test_map", group: Some(McpToolDispatchGroup::Health), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_gini", group: Some(McpToolDispatchGroup::Health), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_dependency_depth", group: Some(McpToolDispatchGroup::Health), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_health", group: Some(McpToolDispatchGroup::Health), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_redundancy", group: Some(McpToolDispatchGroup::Health), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_runtime", group: Some(McpToolDispatchGroup::Health), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_dsm", group: Some(McpToolDispatchGroup::Health), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_test_risk", group: Some(McpToolDispatchGroup::Health), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_automation_run_artifact_view", group: Some(McpToolDispatchGroup::Memory), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_analytics", group: Some(McpToolDispatchGroup::Memory), project: RegisteredProjectAccess::SelectorOnly },
    McpToolBinding { name: "tracedecay_skill_list", group: Some(McpToolDispatchGroup::Memory), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_skill_view", group: Some(McpToolDispatchGroup::Memory), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_hermes_skill_bridge", group: Some(McpToolDispatchGroup::Memory), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_diagnose", group: Some(McpToolDispatchGroup::SessionWorkflow), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_run_affected_tests", group: Some(McpToolDispatchGroup::SessionWorkflow), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_dashboard", group: Some(McpToolDispatchGroup::SessionWorkflow), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_affected_tests", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_code_callees", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_code_callers", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_code_declaration", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_code_definition", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_code_exact_occurrence", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_code_facets", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_code_implementations", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_code_phrase_search", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_code_references", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_code_signature_search", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_code_symbol_search", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_code_timeline", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_code_type_definition", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_code_type_hierarchy", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_configuration_audit", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_configuration_batch", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_configuration_explain", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_configuration_get", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_configuration_list", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_configuration_observed_state", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_configuration_protected_apply", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_configuration_protected_preview", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_configuration_rollback_apply", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_configuration_rollback_preview", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_configuration_set", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_configuration_unset", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_configuration_write_credential", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_context_scout_budget", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_context_scout_cancel", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_context_scout_capability", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_context_scout_claim", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_context_scout_delivery", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_context_scout_explain", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_context_scout_feedback", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_context_scout_pause", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_context_scout_recent", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_context_scout_resume", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_context_scout_status", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_diagnostics_read", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_fact_feedback", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_feedback_advisory_cycle", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_feedback_diagnostics", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_feedback_expand", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_feedback_get", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_feedback_impact", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_feedback_list", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_file_metadata", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_git_apply", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_git_blame", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_git_diff", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_git_history", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_git_hunks", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_git_preview", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_git_status", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_health_delta", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_health_read", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_lcm_compress", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_lcm_describe", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_lcm_doctor", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_lcm_expand", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_lcm_expand_query", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_lcm_grep", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_lcm_load_session", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_lcm_preflight", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_lcm_session_boundary", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_lcm_status", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_module_api", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_qualified_name", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_session_end", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_session_lookup", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_session_refresh", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_session_start", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_sessions_for", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_source_body", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_source_lines", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_source_outline", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_storage_status", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_test_results", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_workflows", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_call_chain", group: None, project: RegisteredProjectAccess::Reader },
    McpToolBinding { name: "tracedecay_file_dependents", group: None, project: RegisteredProjectAccess::Reader },
    McpToolBinding { name: "tracedecay_fact_store", group: None, project: RegisteredProjectAccess::SelectorOnly },
    McpToolBinding { name: "tracedecay_memory_status", group: None, project: RegisteredProjectAccess::SelectorOnly },
    McpToolBinding { name: "tracedecay_message_search", group: None, project: RegisteredProjectAccess::SelectorOnly },];

/// Resolves a tool name against [`MCP_TOOL_BINDINGS`].
///
/// Every dispatched tool call asks this two or three times, so the table is
/// indexed by name once per process rather than scanned each time.
/// `MCP_TOOL_BINDINGS` stays the authority for the rows: a duplicate name
/// would collapse in the index, which `every_tool_is_bound_once` forbids.
fn binding(tool_name: &str) -> Option<&'static McpToolBinding> {
    static BY_NAME: LazyLock<HashMap<&'static str, &'static McpToolBinding>> =
        LazyLock::new(|| {
            MCP_TOOL_BINDINGS
                .iter()
                .map(|binding| (binding.name, binding))
                .collect()
        });
    BY_NAME.get(tool_name).copied()
}

/// The statically bound dispatch group, if this tool has one.
pub(crate) fn dispatch_group_for_tool(tool_name: &str) -> Option<McpToolDispatchGroup> {
    binding(tool_name).and_then(|binding| binding.group)
}

pub(super) fn tool_accepts_registered_project_selector(tool_name: &str) -> bool {
    matches!(
        binding(tool_name).map(|binding| binding.project),
        Some(RegisteredProjectAccess::SelectorOnly | RegisteredProjectAccess::Reader)
    )
}

pub(crate) fn tool_dispatches_registered_project_reader(tool_name: &str) -> bool {
    matches!(
        binding(tool_name).map(|binding| binding.project),
        Some(RegisteredProjectAccess::Reader)
    )
}

fn direct_effect(tool_name: &str) -> EffectClass {
    match tool_name {
        "tracedecay_str_replace"
        | "tracedecay_multi_str_replace"
        | "tracedecay_insert_at"
        | "tracedecay_ast_grep_rewrite"
        | "tracedecay_replace_symbol"
        | "tracedecay_insert_at_symbol"
        | "tracedecay_move_symbol"
        | "tracedecay_api_migration_apply" => EffectClass::SourceEdit,
        "tracedecay_source_edit_reconcile"
        | "tracedecay_dashboard"
        | "tracedecay_fact_store"
        | "tracedecay_fact_feedback"
        | "tracedecay_memory_status"
        | "tracedecay_session_refresh"
        | "tracedecay_run_affected_tests"
        | "tracedecay_lcm_doctor"
        | "tracedecay_lcm_compress"
        | "tracedecay_lcm_session_boundary"
        | "tracedecay_session_start"
        | "tracedecay_session_end" => EffectClass::Administrative,
        _ => EffectClass::Read,
    }
}

fn application_capability_for_tool(
    tool_name: &str,
) -> Result<
    Option<&'static tracedecay_tool_catalog::CapabilityManifestV1>,
    super::dispatch::McpDispatchMetadataError,
> {
    let operation = tool_name.strip_prefix("tracedecay_").unwrap_or(tool_name);
    let catalog = crate::application_surface::application_surface_catalog_ref()?;
    Ok(catalog.capabilities().find(|capability| {
        capability.binding_ids().iter().any(|binding_id| {
            catalog.binding(binding_id).is_some_and(|binding| {
                binding.surface() == BindingSurface::Mcp
                    && binding.operation().as_str() == operation
            })
        })
    }))
}

pub(crate) fn tool_supports_live_cancellation(tool_name: &str) -> bool {
    crate::application_surface::ApplicationSurfaceOperation::from_tool_name(tool_name).is_some()
        || matches!(direct_effect(tool_name), EffectClass::SourceEdit)
        || matches!(
            tool_name,
            "tracedecay_search" | "tracedecay_run_affected_tests"
        )
}

fn verified_effect_journey(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "tracedecay_dashboard"
            | "tracedecay_fact_store"
            | "tracedecay_session_start"
            | "tracedecay_session_end"
    )
}

fn inverse_for_tool(tool_name: &str, effect: EffectClass) -> McpInverseContract {
    if effect.is_read_only() {
        McpInverseContract::NotApplicable
    } else {
        match tool_name {
            "tracedecay_dashboard" => McpInverseContract::SameTool {
                action: "stop".to_owned(),
            },
            "tracedecay_fact_store" => McpInverseContract::SameTool {
                action: "remove".to_owned(),
            },
            "tracedecay_session_start" => McpInverseContract::Tool {
                tool_name: "tracedecay_session_end".to_owned(),
            },
            _ => McpInverseContract::Unavailable {
                reason: McpInverseUnavailableReason::NoVerifiedInverse,
            },
        }
    }
}

fn idempotency_for_tool(tool_name: &str) -> McpIdempotencyContract {
    match tool_name {
        "tracedecay_dashboard" => McpIdempotencyContract::Idempotent,
        "tracedecay_str_replace"
        | "tracedecay_multi_str_replace"
        | "tracedecay_insert_at"
        | "tracedecay_ast_grep_rewrite"
        | "tracedecay_replace_symbol"
        | "tracedecay_insert_at_symbol"
        | "tracedecay_move_symbol"
        | "tracedecay_api_migration_apply" => McpIdempotencyContract::KeyRequired,
        _ => McpIdempotencyContract::NotProvided,
    }
}

fn cancellation_for_tool(
    tool_name: &str,
    application_capability: Option<&tracedecay_tool_catalog::CapabilityManifestV1>,
) -> Result<CancellationContract, tracedecay_tool_catalog::CatalogValidationError> {
    if let Some(capability) = application_capability
        && tool_supports_live_cancellation(tool_name)
    {
        return Ok(capability.cancellation().clone());
    }
    let points = match direct_effect(tool_name) {
        EffectClass::SourceEdit => vec![
            CancellationPoint::BeforeEffect,
            CancellationPoint::EffectInFlight,
        ],
        _ if tool_name == "tracedecay_search" => vec![CancellationPoint::DuringRead],
        _ if tool_name == "tracedecay_run_affected_tests" => {
            vec![CancellationPoint::EffectInFlight]
        }
        _ => return Ok(CancellationContract::NotCancellable),
    };
    CancellationContract::cooperative(points)
}

fn build_mcp_dispatch_catalog()
-> Result<McpDispatchCatalogV1, super::dispatch::McpDispatchMetadataError> {
    let mut contracts = Vec::new();
    for binding in MCP_TOOL_BINDINGS
        .iter()
        .filter(|binding| !super::handlers::INTERNAL_DAEMON_TOOL_NAMES.contains(&binding.name))
    {
        let application_capability = application_capability_for_tool(binding.name)?;
        let direct_effect = direct_effect(binding.name);
        let effect = if direct_effect.is_effect() {
            direct_effect
        } else {
            application_capability.map_or(
                EffectClass::Read,
                tracedecay_tool_catalog::CapabilityManifestV1::effect,
            )
        };
        let available = effect.is_read_only() || verified_effect_journey(binding.name);
        let cancellation = cancellation_for_tool(binding.name, application_capability)?;
        let mut terminal_states = vec![
            McpTerminalState::Completed,
            McpTerminalState::DeadlineExceeded,
            McpTerminalState::Denied,
            McpTerminalState::Failed,
            McpTerminalState::Unavailable,
        ];
        if tool_supports_live_cancellation(binding.name) {
            terminal_states.push(McpTerminalState::Cancelled);
        }
        let streaming = application_capability
            .map(tracedecay_tool_catalog::CapabilityManifestV1::streaming)
            .filter(|streaming| streaming.is_supported())
            .cloned();
        contracts.push(McpDispatchContractV1::new(McpDispatchContractInputV1 {
            tool_name: binding.name.to_owned(),
            availability: if available {
                McpDispatchAvailability::Available
            } else {
                McpDispatchAvailability::Unavailable {
                    reason: McpDispatchUnavailableReason::EffectJourneyUnverified,
                    retryable: false,
                }
            },
            effect,
            deadline: McpDeadlineContractV1::new(
                super::handlers::tool_dispatch_ceiling(binding.name).as_millis() as u64,
            )?,
            idempotency: idempotency_for_tool(binding.name),
            inverse: inverse_for_tool(binding.name, effect),
            cancellation,
            terminal_states,
            pagination: application_capability
                .and_then(tracedecay_tool_catalog::CapabilityManifestV1::pagination)
                .cloned(),
            streaming,
        })?);
    }
    Ok(McpDispatchCatalogV1::new(contracts)?)
}

pub(crate) fn mcp_dispatch_catalog()
-> Result<&'static McpDispatchCatalogV1, super::dispatch::McpDispatchMetadataError> {
    static CATALOG: LazyLock<Result<McpDispatchCatalogV1, String>> =
        LazyLock::new(|| build_mcp_dispatch_catalog().map_err(|error| error.to_string()));
    match &*CATALOG {
        Ok(catalog) => Ok(catalog),
        Err(error) => Err(super::dispatch::McpDispatchMetadataError::Initialization(
            error.clone(),
        )),
    }
}

pub(crate) fn mcp_dispatch_contract(
    tool_name: &str,
) -> Result<&'static McpDispatchContractV1, super::dispatch::McpDispatchMetadataError> {
    mcp_dispatch_catalog()?.contract(tool_name).ok_or_else(|| {
        super::dispatch::McpDispatchMetadataError::MissingContract(tool_name.to_owned())
    })
}

/// Tools whose schema advertises a registered-project selector.
pub(super) fn registered_project_reader_tool_names() -> Vec<&'static str> {
    MCP_TOOL_BINDINGS
        .iter()
        .filter(|entry| entry.project == RegisteredProjectAccess::Reader)
        .map(|entry| entry.name)
        .collect()
}

#[cfg(test)]
mod tests {
    use tracedecay_application::RetainedSurfaceOperation;

    use super::*;
    use crate::application_surface::ApplicationSurfaceOperation;

    #[test]
    fn every_tool_is_bound_once() {
        let mut names: Vec<&str> = MCP_TOOL_BINDINGS.iter().map(|entry| entry.name).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "a tool name is bound twice");
    }

    #[test]
    fn every_maximal_tool_definition_has_a_binding() {
        let defined = super::super::definitions::get_maximal_tool_definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect::<std::collections::BTreeSet<_>>();
        let bound = MCP_TOOL_BINDINGS
            .iter()
            .filter(|binding| {
                !super::super::handlers::INTERNAL_DAEMON_TOOL_NAMES.contains(&binding.name)
            })
            .map(|binding| binding.name.to_owned())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            defined, bound,
            "MCP definitions and dispatch bindings must be a bijection"
        );
    }

    #[test]
    fn dispatch_catalog_covers_every_advertised_binding_with_canonical_deadline() {
        let catalog = mcp_dispatch_catalog().unwrap();
        let advertised = MCP_TOOL_BINDINGS
            .iter()
            .filter(|binding| {
                !super::super::handlers::INTERNAL_DAEMON_TOOL_NAMES.contains(&binding.name)
            })
            .map(|binding| binding.name)
            .collect::<std::collections::BTreeSet<_>>();
        let cataloged = catalog
            .contracts()
            .map(tracedecay_tool_catalog::McpDispatchContractV1::tool_name)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(cataloged, advertised);
        for contract in catalog.contracts() {
            assert_eq!(
                contract.deadline().maximum_millis(),
                super::super::handlers::tool_dispatch_ceiling(contract.tool_name()).as_millis()
                    as u64
            );
            assert_eq!(
                contract.availability().is_available(),
                contract.effect().is_read_only() || verified_effect_journey(contract.tool_name())
            );
        }
    }

    #[test]
    fn mixed_repair_tools_are_effects_without_fabricated_lifecycle_claims() {
        let catalog = mcp_dispatch_catalog().unwrap();
        for tool_name in [
            "tracedecay_lcm_doctor",
            "tracedecay_memory_status",
            "tracedecay_session_refresh",
        ] {
            let contract = catalog.contract(tool_name).unwrap();
            assert_eq!(contract.effect(), EffectClass::Administrative);
            assert!(!contract.read_only());
            assert!(!contract.availability().is_available());
            assert_eq!(contract.idempotency(), McpIdempotencyContract::NotProvided);
            assert!(matches!(
                contract.inverse(),
                McpInverseContract::Unavailable { .. }
            ));
        }
    }

    #[test]
    fn direct_cancellation_contracts_name_observed_handler_stages() {
        let catalog = mcp_dispatch_catalog().unwrap();
        assert_eq!(
            catalog
                .contract("tracedecay_search")
                .unwrap()
                .cancellation()
                .points(),
            &[CancellationPoint::DuringRead]
        );
        assert_eq!(
            catalog
                .contract("tracedecay_run_affected_tests")
                .unwrap()
                .cancellation()
                .points(),
            &[CancellationPoint::EffectInFlight]
        );
    }

    /// Reads that resolve their own authority stay on the active project. A
    /// selector on one of these would silently read the wrong store.
    ///
    /// Only names with a `MCP_TOOL_BINDINGS` row belong here: for an unbound
    /// name both predicates return `false` vacuously, so listing one asserts
    /// nothing. The `tracedecay_git_*` application-surface tools were removed
    /// for exactly that reason — they never consult this table, and their
    /// selector policy is enforced by the surface schema, not a binding row.
    #[test]
    fn authority_bound_reads_are_active_project_only() {
        let tool_name = "tracedecay_search";
        assert!(
            binding(tool_name).is_some(),
            "{tool_name} must have a binding row for these assertions to bind"
        );
        assert!(!tool_accepts_registered_project_selector(tool_name));
        assert!(!tool_dispatches_registered_project_reader(tool_name));
    }

    /// A row without a group must be claimed by one of the surface predicates,
    /// otherwise the tool would reach dispatch with no owner at all.
    #[test]
    fn group_less_rows_are_owned_by_a_surface_predicate() {
        for entry in MCP_TOOL_BINDINGS
            .iter()
            .filter(|entry| entry.group.is_none())
        {
            let claimed = ApplicationSurfaceOperation::from_tool_name(entry.name).is_some()
                || RetainedSurfaceOperation::from_name(entry.name).is_some();
            assert!(claimed, "{} has no group and no surface owner", entry.name);
        }
    }

    /// The retained-surface predicate used to sit between the health and memory
    /// arms of an ordered match, so retained tools won over those two groups.
    /// A flat lookup only preserves that if no memory or session-workflow tool
    /// is also a retained operation.
    #[test]
    fn memory_and_session_workflow_tools_are_not_retained_operations() {
        for entry in MCP_TOOL_BINDINGS.iter().filter(|entry| {
            matches!(
                entry.group,
                Some(McpToolDispatchGroup::Memory | McpToolDispatchGroup::SessionWorkflow)
            )
        }) {
            assert!(
                RetainedSurfaceOperation::from_name(entry.name).is_none(),
                "{} would change groups under a flat lookup",
                entry.name
            );
        }
    }
}
