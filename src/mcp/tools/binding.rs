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
