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

use tracedecay_tool_catalog::{DeadlineBehavior, EffectClass};

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

/// Deadline and effect class for root-owned compatibility handlers.
///
/// Catalog-owned application and retained operations resolve their execution
/// contract from the catalog. These rows are the remaining executable
/// authority until each compatibility handler is migrated; putting the class
/// on the binding makes an extended deadline a declared property rather than
/// a second name-matching ceiling in dispatch code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LegacyMcpToolExecutionClass {
    InteractiveRead,
    InteractivePreview,
    SourceEdit,
    ExtendedAdministrative,
}

impl LegacyMcpToolExecutionClass {
    const fn for_group(group: Option<McpToolDispatchGroup>) -> Self {
        match group {
            Some(McpToolDispatchGroup::Edit) => Self::SourceEdit,
            Some(McpToolDispatchGroup::Admin) => Self::ExtendedAdministrative,
            _ => Self::InteractiveRead,
        }
    }

    pub(crate) const fn effect(self) -> EffectClass {
        match self {
            Self::InteractiveRead => EffectClass::Read,
            Self::InteractivePreview => EffectClass::Preview,
            Self::SourceEdit => EffectClass::SourceEdit,
            Self::ExtendedAdministrative => EffectClass::Administrative,
        }
    }

    pub(crate) const fn deadline_millis(self) -> u64 {
        match self {
            Self::SourceEdit => 30_000,
            Self::ExtendedAdministrative => 600_000,
            Self::InteractiveRead | Self::InteractivePreview => 120_000,
        }
    }

    pub(crate) const fn deadline_behavior(self) -> DeadlineBehavior {
        match self {
            Self::SourceEdit | Self::ExtendedAdministrative => {
                DeadlineBehavior::ReturnEffectReceipt
            }
            Self::InteractiveRead | Self::InteractivePreview => {
                DeadlineBehavior::ReturnOperationReceipt
            }
        }
    }
}

pub(crate) struct McpToolBinding {
    pub(crate) name: &'static str,
    pub(crate) group: Option<McpToolDispatchGroup>,
    pub(crate) project: RegisteredProjectAccess,
    pub(crate) execution: LegacyMcpToolExecutionClass,
}

macro_rules! legacy_binding {
    ($name:literal, $group:expr, $project:expr) => {
        McpToolBinding {
            name: $name,
            group: $group,
            project: $project,
            execution: LegacyMcpToolExecutionClass::for_group($group),
        }
    };
    ($name:literal, $group:expr, $project:expr, $execution:expr) => {
        McpToolBinding {
            name: $name,
            group: $group,
            project: $project,
            execution: $execution,
        }
    };
}

#[rustfmt::skip]
pub(crate) const MCP_TOOL_BINDINGS: &[McpToolBinding] = &[
    legacy_binding!("tracedecay_search", Some(McpToolDispatchGroup::Graph), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_grep", Some(McpToolDispatchGroup::Graph), RegisteredProjectAccess::Reader),
    legacy_binding!("tracedecay_ast_grep_search", Some(McpToolDispatchGroup::Graph), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_retrieve", Some(McpToolDispatchGroup::Graph), RegisteredProjectAccess::Reader),
    legacy_binding!("tracedecay_context", Some(McpToolDispatchGroup::Graph), RegisteredProjectAccess::Reader),
    legacy_binding!("tracedecay_callers", Some(McpToolDispatchGroup::Graph), RegisteredProjectAccess::Reader),
    legacy_binding!("tracedecay_callees", Some(McpToolDispatchGroup::Graph), RegisteredProjectAccess::Reader),
    legacy_binding!("tracedecay_impact", Some(McpToolDispatchGroup::Graph), RegisteredProjectAccess::Reader),
    legacy_binding!("tracedecay_node", Some(McpToolDispatchGroup::Graph), RegisteredProjectAccess::Reader),
    legacy_binding!("tracedecay_similar", Some(McpToolDispatchGroup::Graph), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_rename_preview", Some(McpToolDispatchGroup::Graph), RegisteredProjectAccess::ActiveProjectOnly, LegacyMcpToolExecutionClass::InteractivePreview),
    legacy_binding!("tracedecay_implementations", Some(McpToolDispatchGroup::Graph), RegisteredProjectAccess::Reader),
    legacy_binding!("tracedecay_callers_for", Some(McpToolDispatchGroup::Graph), RegisteredProjectAccess::Reader),
    legacy_binding!("tracedecay_find_exact_symbol", Some(McpToolDispatchGroup::Graph), RegisteredProjectAccess::Reader),
    legacy_binding!("tracedecay_by_qualified_name", Some(McpToolDispatchGroup::Graph), RegisteredProjectAccess::Reader),
    legacy_binding!("tracedecay_signature", Some(McpToolDispatchGroup::Graph), RegisteredProjectAccess::Reader),
    legacy_binding!("tracedecay_impls", Some(McpToolDispatchGroup::Graph), RegisteredProjectAccess::Reader),
    legacy_binding!("tracedecay_derives", Some(McpToolDispatchGroup::Graph), RegisteredProjectAccess::Reader),
    legacy_binding!("tracedecay_status", Some(McpToolDispatchGroup::Info), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_active_project", Some(McpToolDispatchGroup::Info), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_project_list", Some(McpToolDispatchGroup::Info), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_project_search", Some(McpToolDispatchGroup::Info), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_project_context", Some(McpToolDispatchGroup::Info), RegisteredProjectAccess::SelectorOnly),
    legacy_binding!("tracedecay_files", Some(McpToolDispatchGroup::Info), RegisteredProjectAccess::Reader),
    legacy_binding!("tracedecay_admin_sync", Some(McpToolDispatchGroup::Info), RegisteredProjectAccess::ActiveProjectOnly, LegacyMcpToolExecutionClass::ExtendedAdministrative),
    legacy_binding!("tracedecay_port_status", Some(McpToolDispatchGroup::Info), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_port_order", Some(McpToolDispatchGroup::Info), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_simplify_scan", Some(McpToolDispatchGroup::Info), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_type_hierarchy", Some(McpToolDispatchGroup::Info), RegisteredProjectAccess::Reader),
    legacy_binding!("tracedecay_body", Some(McpToolDispatchGroup::Info), RegisteredProjectAccess::Reader),
    legacy_binding!("tracedecay_todos", Some(McpToolDispatchGroup::Info), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_read", Some(McpToolDispatchGroup::Info), RegisteredProjectAccess::Reader),
    legacy_binding!("tracedecay_outline", Some(McpToolDispatchGroup::Info), RegisteredProjectAccess::Reader),
    legacy_binding!("tracedecay_config", Some(McpToolDispatchGroup::Info), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_signature_search", Some(McpToolDispatchGroup::Info), RegisteredProjectAccess::Reader),
    legacy_binding!("tracedecay_hook_runtime", Some(McpToolDispatchGroup::Admin), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_admin_cli", Some(McpToolDispatchGroup::Admin), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_admin_project", Some(McpToolDispatchGroup::Admin), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_dead_code", Some(McpToolDispatchGroup::Analysis), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_circular", Some(McpToolDispatchGroup::Analysis), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_hotspots", Some(McpToolDispatchGroup::Analysis), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_unused_imports", Some(McpToolDispatchGroup::Analysis), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_rank", Some(McpToolDispatchGroup::Analysis), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_largest", Some(McpToolDispatchGroup::Analysis), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_coupling", Some(McpToolDispatchGroup::Analysis), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_inheritance_depth", Some(McpToolDispatchGroup::Analysis), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_distribution", Some(McpToolDispatchGroup::Analysis), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_recursion", Some(McpToolDispatchGroup::Analysis), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_complexity", Some(McpToolDispatchGroup::Analysis), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_doc_coverage", Some(McpToolDispatchGroup::Analysis), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_god_class", Some(McpToolDispatchGroup::Analysis), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_unsafe_patterns", Some(McpToolDispatchGroup::Analysis), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_constructors", Some(McpToolDispatchGroup::Analysis), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_field_sites", Some(McpToolDispatchGroup::Analysis), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_diagnostics", Some(McpToolDispatchGroup::Analysis), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_admin_branch_add", Some(McpToolDispatchGroup::Git), RegisteredProjectAccess::ActiveProjectOnly, LegacyMcpToolExecutionClass::ExtendedAdministrative),
    legacy_binding!("tracedecay_affected", Some(McpToolDispatchGroup::Git), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_diff_context", Some(McpToolDispatchGroup::Git), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_changelog", Some(McpToolDispatchGroup::Git), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_commit_context", Some(McpToolDispatchGroup::Git), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_pr_context", Some(McpToolDispatchGroup::Git), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_branch_search", Some(McpToolDispatchGroup::Git), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_branch_diff", Some(McpToolDispatchGroup::Git), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_branch_list", Some(McpToolDispatchGroup::Git), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_str_replace", Some(McpToolDispatchGroup::Edit), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_multi_str_replace", Some(McpToolDispatchGroup::Edit), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_insert_at", Some(McpToolDispatchGroup::Edit), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_ast_grep_rewrite", Some(McpToolDispatchGroup::Edit), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_replace_symbol", Some(McpToolDispatchGroup::Edit), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_insert_at_symbol", Some(McpToolDispatchGroup::Edit), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_move_symbol", Some(McpToolDispatchGroup::Edit), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_api_migration_plan", Some(McpToolDispatchGroup::Edit), RegisteredProjectAccess::ActiveProjectOnly, LegacyMcpToolExecutionClass::InteractivePreview),
    legacy_binding!("tracedecay_api_migration_apply", Some(McpToolDispatchGroup::Edit), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_source_edit_reconcile", Some(McpToolDispatchGroup::Edit), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_test_map", Some(McpToolDispatchGroup::Health), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_gini", Some(McpToolDispatchGroup::Health), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_dependency_depth", Some(McpToolDispatchGroup::Health), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_health", Some(McpToolDispatchGroup::Health), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_redundancy", Some(McpToolDispatchGroup::Health), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_runtime", Some(McpToolDispatchGroup::Health), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_dsm", Some(McpToolDispatchGroup::Health), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_test_risk", Some(McpToolDispatchGroup::Health), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_automation_run_artifact_view", Some(McpToolDispatchGroup::Memory), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_analytics", Some(McpToolDispatchGroup::Memory), RegisteredProjectAccess::SelectorOnly),
    legacy_binding!("tracedecay_skill_list", Some(McpToolDispatchGroup::Memory), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_skill_view", Some(McpToolDispatchGroup::Memory), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_hermes_skill_bridge", Some(McpToolDispatchGroup::Memory), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_diagnose", Some(McpToolDispatchGroup::SessionWorkflow), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_run_affected_tests", Some(McpToolDispatchGroup::SessionWorkflow), RegisteredProjectAccess::ActiveProjectOnly, LegacyMcpToolExecutionClass::ExtendedAdministrative),
    legacy_binding!("tracedecay_dashboard", Some(McpToolDispatchGroup::SessionWorkflow), RegisteredProjectAccess::ActiveProjectOnly),
    legacy_binding!("tracedecay_call_chain", None, RegisteredProjectAccess::Reader),
    legacy_binding!("tracedecay_file_dependents", None, RegisteredProjectAccess::Reader),
    legacy_binding!("tracedecay_fact_store", None, RegisteredProjectAccess::SelectorOnly),
    legacy_binding!("tracedecay_memory_status", None, RegisteredProjectAccess::SelectorOnly),
    legacy_binding!("tracedecay_message_search", None, RegisteredProjectAccess::SelectorOnly),];

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

/// Root-owned compatibility execution metadata for a statically bound tool.
pub(crate) fn legacy_execution_class(tool_name: &str) -> Option<LegacyMcpToolExecutionClass> {
    binding(tool_name).map(|binding| binding.execution)
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
