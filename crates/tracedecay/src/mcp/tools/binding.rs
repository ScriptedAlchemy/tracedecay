//! Canonical binding between an MCP tool name and how the server treats it.
//!
//! The root-owned table answers both questions the dispatcher and schema layer
//! used to answer from separate name lists: which dispatch group owns a tool,
//! and whether it accepts a registered-project selector. Work is intentionally
//! projected from its executable registry instead, because that registry owns
//! its complete mounted operation set and lifecycle contracts.
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

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use tracedecay_application::RetainedSurfaceOperation;
use tracedecay_application::multi_root::{
    MultiRootApplicationOperation, multi_root_capability_manifest,
};
use tracedecay_tool_catalog::{
    ApplicationSurfaceOperation, BindingSurface, CancellationContract, CancellationPoint,
    EffectClass, ExecutableBindingV1, McpDeadlineContractV1, McpDispatchAvailability,
    McpDispatchCatalogV1, McpDispatchContractInputV1, McpDispatchContractV1,
    McpDispatchUnavailableReason, McpIdempotencyContract, McpInverseContract,
    McpInverseUnavailableReason, McpTerminalState,
};

mod work;
mod workflow;

use work::work_executable_binding_for_tool;
pub(crate) use work::work_operation_for_tool;
use workflow::workflow_executable_binding_for_tool;
pub(crate) use workflow::workflow_operation_for_tool;

/// Which dispatch family owns a tool once the surface predicates decline it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum McpToolDispatchGroup {
    ApplicationSurface,
    MultiRoot,
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
    Work,
    Workflow,
}

/// Whether a tool's authority depends on the live checked-out branch.
///
/// Kept internal: the `tracedecay/dispatch` contract has no field family for
/// this policy, and advertising it would freeze a request-routing concern
/// into the wire catalog.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BranchSensitivity {
    /// Reads the code graph, project files, or git. Must detect a checkout
    /// on the next request (today's `reopen_if_branch_drifted_memoized` path).
    Sensitive,
    /// Retained memory, configuration, work, and workflow authority. The
    /// serving snapshot is enough; a live HEAD probe cannot change the answer.
    Independent,
}

/// Classifies a dispatched tool's dependence on the live git branch.
///
/// Unknown and unlisted names are [`BranchSensitivity::Sensitive`] so a newly
/// advertised tool keeps today's drift detection until it is classified.
pub(crate) fn tool_branch_sensitivity(tool_name: &str) -> BranchSensitivity {
    if let Some(operation) = ApplicationSurfaceOperation::from_tool_name(tool_name) {
        return application_surface_branch_sensitivity(operation);
    }
    match dispatch_group_for_tool(tool_name) {
        Some(
            McpToolDispatchGroup::Graph
            | McpToolDispatchGroup::Info
            | McpToolDispatchGroup::Admin
            | McpToolDispatchGroup::Analysis
            | McpToolDispatchGroup::Git
            | McpToolDispatchGroup::Edit
            | McpToolDispatchGroup::Health
            | McpToolDispatchGroup::MultiRoot
            | McpToolDispatchGroup::ApplicationSurface
            | McpToolDispatchGroup::SessionWorkflow,
        ) => BranchSensitivity::Sensitive,
        Some(
            McpToolDispatchGroup::RetainedApplication
            | McpToolDispatchGroup::Memory
            | McpToolDispatchGroup::Work
            | McpToolDispatchGroup::Workflow,
        ) => BranchSensitivity::Independent,
        None => {
            if RetainedSurfaceOperation::from_tool_name(tool_name).is_some() {
                BranchSensitivity::Independent
            } else {
                BranchSensitivity::Sensitive
            }
        }
    }
}

fn application_surface_branch_sensitivity(
    operation: ApplicationSurfaceOperation,
) -> BranchSensitivity {
    use ApplicationSurfaceOperation::{
        AffectedTests, CallChain, CodeCallees, CodeCallers, CodeDeclaration, CodeDefinition,
        CodeExactOccurrence, CodeFacets, CodeImplementations, CodePhraseSearch, CodeReferences,
        CodeSignatureSearch, CodeSymbolSearch, CodeTimeline, CodeTypeDefinition, CodeTypeHierarchy,
        ConfigurationAudit, ConfigurationBatch, ConfigurationExplain, ConfigurationGet,
        ConfigurationList, ConfigurationObservedState, ConfigurationProtectedApply,
        ConfigurationProtectedPreview, ConfigurationRollbackApply, ConfigurationRollbackPreview,
        ConfigurationSet, ConfigurationUnset, ConfigurationWriteCredential, ContextScoutBudget,
        ContextScoutCancel, ContextScoutCapability, ContextScoutClaim, ContextScoutDelivery,
        ContextScoutExplain, ContextScoutFeedback, ContextScoutPause, ContextScoutRecent,
        ContextScoutResume, ContextScoutStatus, DiagnosticsRead, FeedbackAdvisoryCycle,
        FeedbackDiagnostics, FeedbackExpand, FeedbackGet, FeedbackImpact, FeedbackList,
        FileDependents, FileMetadata, GitApply, GitBlame, GitDiff, GitHistory,
        GitHubStackSignalExpand, GitHunks, GitPreview, GitStatus, HealthDelta, HealthRead,
        ModuleApi, NativeIntegrationApply, NativeIntegrationApprove, NativeIntegrationCancel,
        NativeIntegrationPreflight, NativeIntegrationStackSnapshot, NativeIntegrationStatus,
        NativeIntegrationWorktreeConfirm, NativeIntegrationWorktreeInspect,
        NativeIntegrationWorktreeInventory, NativeIntegrationWorktreeReconcile,
        NativeIntegrationWorktreeRemove, ObservatoryRead, QualifiedName, SessionLookup, SourceBody,
        SourceLines, SourceOutline, StorageStatus, TestResults,
    };
    match operation {
        // Mixed ApplicationSurface group: these operations read configuration,
        // host-integration lifecycle, session identity, store identity, or
        // process observability — never the checkout, code graph, or files.
        ConfigurationList
        | ConfigurationExplain
        | ConfigurationGet
        | ConfigurationSet
        | ConfigurationUnset
        | ConfigurationBatch
        | ConfigurationWriteCredential
        | ConfigurationObservedState
        | ConfigurationProtectedPreview
        | ConfigurationProtectedApply
        | ConfigurationRollbackPreview
        | ConfigurationRollbackApply
        | ConfigurationAudit
        | ContextScoutStatus
        | ContextScoutRecent
        | ContextScoutExplain
        | ContextScoutCapability
        | ContextScoutBudget
        | ContextScoutPause
        | ContextScoutResume
        | ContextScoutCancel
        | ContextScoutClaim
        | ContextScoutDelivery
        | ContextScoutFeedback
        | SessionLookup
        | StorageStatus
        | ObservatoryRead
        | NativeIntegrationPreflight
        | NativeIntegrationApprove
        | NativeIntegrationApply
        | NativeIntegrationStatus
        | NativeIntegrationCancel => BranchSensitivity::Independent,
        // Mixed ApplicationSurface group: git walks, worktree inventory, stack
        // snapshots, code-graph reads, source-file bodies, health, diagnostics,
        // and post-edit feedback all depend on the current checkout or graph.
        GitStatus
        | GitDiff
        | GitHistory
        | GitBlame
        | GitHunks
        | GitPreview
        | GitApply
        | GitHubStackSignalExpand
        | NativeIntegrationStackSnapshot
        | NativeIntegrationWorktreeInventory
        | NativeIntegrationWorktreeInspect
        | NativeIntegrationWorktreeConfirm
        | NativeIntegrationWorktreeRemove
        | NativeIntegrationWorktreeReconcile
        | FeedbackDiagnostics
        | FeedbackGet
        | FeedbackExpand
        | FeedbackList
        | FeedbackImpact
        | FeedbackAdvisoryCycle
        | AffectedTests
        | TestResults
        | CodeExactOccurrence
        | CodePhraseSearch
        | CodeSymbolSearch
        | CodeSignatureSearch
        | CodeImplementations
        | CodeTypeHierarchy
        | CodeCallers
        | CodeCallees
        | CodeFacets
        | CodeTimeline
        | CodeDeclaration
        | CodeDefinition
        | CodeTypeDefinition
        | CodeReferences
        | QualifiedName
        | CallChain
        | FileDependents
        | SourceLines
        | SourceBody
        | SourceOutline
        | ModuleApi
        | FileMetadata
        | HealthRead
        | HealthDelta
        | DiagnosticsRead => BranchSensitivity::Sensitive,
    }
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
const MCP_TOOL_BINDING_SPECS: &[McpToolBinding] = &[
    McpToolBinding { name: "tracedecay_search", group: Some(McpToolDispatchGroup::Graph), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_grep", group: Some(McpToolDispatchGroup::Graph), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_ast_grep_search", group: Some(McpToolDispatchGroup::Graph), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_retrieve", group: Some(McpToolDispatchGroup::Graph), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_context", group: Some(McpToolDispatchGroup::Graph), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_callers", group: Some(McpToolDispatchGroup::Graph), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_callees", group: Some(McpToolDispatchGroup::Graph), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_impact", group: Some(McpToolDispatchGroup::Graph), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_node", group: Some(McpToolDispatchGroup::Graph), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_similar", group: Some(McpToolDispatchGroup::Graph), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_rename_preview", group: Some(McpToolDispatchGroup::Graph), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_implementations", group: Some(McpToolDispatchGroup::Graph), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_callers_for", group: Some(McpToolDispatchGroup::Graph), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_find_exact_symbol", group: Some(McpToolDispatchGroup::Graph), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_by_qualified_name", group: Some(McpToolDispatchGroup::Graph), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_signature", group: Some(McpToolDispatchGroup::Graph), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_impls", group: Some(McpToolDispatchGroup::Graph), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_derives", group: Some(McpToolDispatchGroup::Graph), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_status", group: Some(McpToolDispatchGroup::Info), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_remote_status", group: Some(McpToolDispatchGroup::Info), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_active_project", group: Some(McpToolDispatchGroup::Info), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_project_list", group: Some(McpToolDispatchGroup::Info), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_project_search", group: Some(McpToolDispatchGroup::Info), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_project_context", group: Some(McpToolDispatchGroup::Info), project: RegisteredProjectAccess::SelectorOnly },
    McpToolBinding { name: "tracedecay_files", group: Some(McpToolDispatchGroup::Info), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_admin_sync", group: Some(McpToolDispatchGroup::Info), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_port_status", group: Some(McpToolDispatchGroup::Info), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_port_order", group: Some(McpToolDispatchGroup::Info), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_simplify_scan", group: Some(McpToolDispatchGroup::Info), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_type_hierarchy", group: Some(McpToolDispatchGroup::Info), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_body", group: Some(McpToolDispatchGroup::Info), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_todos", group: Some(McpToolDispatchGroup::Info), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_read", group: Some(McpToolDispatchGroup::Info), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_outline", group: Some(McpToolDispatchGroup::Info), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_config", group: Some(McpToolDispatchGroup::Info), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_signature_search", group: Some(McpToolDispatchGroup::Info), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_hook_runtime", group: Some(McpToolDispatchGroup::Admin), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_admin_cli", group: Some(McpToolDispatchGroup::Admin), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_admin_project", group: Some(McpToolDispatchGroup::Admin), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_dead_code", group: Some(McpToolDispatchGroup::Analysis), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_circular", group: Some(McpToolDispatchGroup::Analysis), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_hotspots", group: Some(McpToolDispatchGroup::Analysis), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_unused_imports", group: Some(McpToolDispatchGroup::Analysis), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_unmounted_files", group: Some(McpToolDispatchGroup::Analysis), project: RegisteredProjectAccess::ActiveProjectOnly },
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
    McpToolBinding { name: "tracedecay_rename_symbol", group: Some(McpToolDispatchGroup::Edit), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_source_edit_reconcile", group: Some(McpToolDispatchGroup::Edit), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_source_edit_rollback", group: Some(McpToolDispatchGroup::Edit), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_test_map", group: Some(McpToolDispatchGroup::Health), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_gini", group: Some(McpToolDispatchGroup::Health), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_dependency_depth", group: Some(McpToolDispatchGroup::Health), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_health", group: Some(McpToolDispatchGroup::Health), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_redundancy", group: Some(McpToolDispatchGroup::Health), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_runtime", group: Some(McpToolDispatchGroup::Health), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_dsm", group: Some(McpToolDispatchGroup::Health), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_test_risk", group: Some(McpToolDispatchGroup::Health), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_automation_run_list", group: Some(McpToolDispatchGroup::Memory), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_automation_run_view", group: Some(McpToolDispatchGroup::Memory), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_automation_run_artifact_view", group: Some(McpToolDispatchGroup::Memory), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_analytics", group: Some(McpToolDispatchGroup::Memory), project: RegisteredProjectAccess::SelectorOnly },
    McpToolBinding { name: "tracedecay_skill_list", group: Some(McpToolDispatchGroup::Memory), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_skill_view", group: Some(McpToolDispatchGroup::Memory), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_hermes_skill_bridge", group: Some(McpToolDispatchGroup::Memory), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_multi_root_scope_set_read", group: Some(McpToolDispatchGroup::MultiRoot), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_multi_root_scope_set_compare_and_swap", group: Some(McpToolDispatchGroup::MultiRoot), project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_multi_root_execute", group: Some(McpToolDispatchGroup::MultiRoot), project: RegisteredProjectAccess::ActiveProjectOnly },
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
    McpToolBinding { name: "tracedecay_fact_feedback", group: None, project: RegisteredProjectAccess::SelectorOnly },
    McpToolBinding { name: "tracedecay_feedback_advisory_cycle", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_feedback_diagnostics", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_feedback_expand", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_feedback_get", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_feedback_impact", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_feedback_list", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_file_metadata", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_git_apply", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_apply_native_integration", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_approve_native_integration", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_cancel_native_integration", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_native_integration_status", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_preflight_native_integration", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_stack_snapshot", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_worktree_inventory", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_worktree_cleanup_inspect", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_worktree_cleanup_confirm", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_worktree_cleanup_remove", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_worktree_cleanup_reconcile", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_git_blame", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_git_diff", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_github_stack_signal_expand", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_git_history", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_git_hunks", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_git_preview", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_git_status", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_health_delta", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_health_read", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_lcm_describe", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_lcm_doctor", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_lcm_expand", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_lcm_expand_query", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_lcm_grep", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_lcm_load_session", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_lcm_status", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_observatory_read", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_module_api", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_qualified_name", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_session_lookup", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_session_refresh", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_session_refresh_begin", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_session_refresh_status", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_session_refresh_cancel", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_sessions_for", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_source_body", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_source_lines", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_source_outline", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_storage_status", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_test_results", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_workflows", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_call_chain", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_file_dependents", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_fact_store_curate", group: None, project: RegisteredProjectAccess::ActiveProjectOnly },
    McpToolBinding { name: "tracedecay_fact_store_add", group: None, project: RegisteredProjectAccess::SelectorOnly },
    McpToolBinding { name: "tracedecay_fact_store_search", group: None, project: RegisteredProjectAccess::SelectorOnly },
    McpToolBinding { name: "tracedecay_fact_store_probe", group: None, project: RegisteredProjectAccess::SelectorOnly },
    McpToolBinding { name: "tracedecay_fact_store_related", group: None, project: RegisteredProjectAccess::SelectorOnly },
    McpToolBinding { name: "tracedecay_fact_store_reason", group: None, project: RegisteredProjectAccess::SelectorOnly },
    McpToolBinding { name: "tracedecay_fact_store_contradict", group: None, project: RegisteredProjectAccess::SelectorOnly },
    McpToolBinding { name: "tracedecay_fact_store_get", group: None, project: RegisteredProjectAccess::SelectorOnly },
    McpToolBinding { name: "tracedecay_fact_store_update", group: None, project: RegisteredProjectAccess::SelectorOnly },
    McpToolBinding { name: "tracedecay_fact_store_remove", group: None, project: RegisteredProjectAccess::SelectorOnly },
    McpToolBinding { name: "tracedecay_fact_store_supersede", group: None, project: RegisteredProjectAccess::SelectorOnly },
    McpToolBinding { name: "tracedecay_fact_store_list", group: None, project: RegisteredProjectAccess::SelectorOnly },
    McpToolBinding { name: "tracedecay_memory_status", group: None, project: RegisteredProjectAccess::SelectorOnly },
    McpToolBinding { name: "tracedecay_message_search", group: None, project: RegisteredProjectAccess::SelectorOnly },
];

pub(crate) static MCP_TOOL_BINDINGS: LazyLock<Vec<McpToolBinding>> =
    LazyLock::new(assemble_mcp_tool_bindings);

fn assemble_mcp_tool_bindings() -> Vec<McpToolBinding> {
    let readers: HashSet<&str> = tracedecay_mcp::registered_project_reader_tool_names()
        .into_iter()
        .collect();
    let mut assigned = HashSet::new();
    let bindings = MCP_TOOL_BINDING_SPECS
        .iter()
        .map(|spec| {
            assert!(
                spec.project != RegisteredProjectAccess::Reader,
                "MCP_TOOL_BINDING_SPECS must not encode Reader for '{}'; list it in tracedecay-mcp::project_access",
                spec.name
            );
            let project = if readers.contains(spec.name) {
                assigned.insert(spec.name);
                RegisteredProjectAccess::Reader
            } else {
                spec.project
            };
            McpToolBinding {
                name: spec.name,
                group: spec.group,
                project,
            }
        })
        .collect();
    let missing: Vec<&str> = readers
        .iter()
        .copied()
        .filter(|name| !assigned.contains(name))
        .collect();
    assert!(
        missing.is_empty(),
        "tracedecay-mcp reader tools have no MCP_TOOL_BINDING_SPECS row: {missing:?}"
    );
    bindings
}

/// Resolves a static MCP tool name against [`MCP_TOOL_BINDINGS`].
///
/// Every dispatched tool call asks this two or three times, so the table is
/// indexed by name once per process rather than scanned each time.
/// `MCP_TOOL_BINDINGS` stays the authority for static rows: a duplicate name
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
    binding(tool_name)
        .and_then(|binding| binding.group)
        .or_else(|| work_operation_for_tool(tool_name).map(|_| McpToolDispatchGroup::Work))
        .or_else(|| workflow_operation_for_tool(tool_name).map(|_| McpToolDispatchGroup::Workflow))
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

/// Selector-bound effects accept a project selector but must not open the
/// selected project's store. The calling session stays admitted; the retained
/// owner denies a foreign selector as `NotFoundOrNotAuthorized`.
#[cfg(test)]
pub(super) fn tool_is_selector_bound_effect(tool_name: &str) -> bool {
    matches!(
        binding(tool_name).map(|binding| binding.project),
        Some(RegisteredProjectAccess::SelectorOnly)
    ) && direct_effect(tool_name).is_effect()
}

fn direct_effect(tool_name: &str) -> EffectClass {
    match tool_name {
        "tracedecay_multi_root_scope_set_compare_and_swap"
        | "tracedecay_dashboard"
        | "tracedecay_fact_store_curate"
        | "tracedecay_fact_store_add"
        | "tracedecay_fact_store_update"
        | "tracedecay_fact_store_remove"
        | "tracedecay_fact_store_supersede"
        | "tracedecay_fact_feedback"
        | "tracedecay_session_refresh"
        | "tracedecay_session_refresh_begin"
        | "tracedecay_session_refresh_cancel"
        | "tracedecay_run_affected_tests" => EffectClass::Administrative,
        _ => EffectClass::Read,
    }
}

/// The multi-root operation a tool name resolves to, if any.
///
/// The three multi-root tools are daemon-owned: they carry no application
/// surface binding, so their contract comes from the multi-root capability
/// catalog rather than [`application_capability_for_tool`].
fn multi_root_operation_for_tool(tool_name: &str) -> Option<MultiRootApplicationOperation> {
    match tool_name {
        "tracedecay_multi_root_scope_set_read" => Some(MultiRootApplicationOperation::ScopeSetRead),
        "tracedecay_multi_root_scope_set_compare_and_swap" => {
            Some(MultiRootApplicationOperation::ScopeSetCompareAndSwap)
        }
        "tracedecay_multi_root_execute" => Some(MultiRootApplicationOperation::Execute),
        _ => None,
    }
}

fn multi_root_capability_for_tool(
    tool_name: &str,
) -> Result<
    Option<tracedecay_tool_catalog::CapabilityManifestV1>,
    super::dispatch::McpDispatchMetadataError,
> {
    multi_root_operation_for_tool(tool_name)
        .map(multi_root_capability_manifest)
        .transpose()
        .map_err(super::dispatch::McpDispatchMetadataError::CatalogValidation)
}

/// One MCP dispatch entry normalized from either the static root-owned binding
/// table or a canonical Work executable binding.
pub(super) struct DispatchCatalogBinding {
    pub(super) name: String,
    pub(super) group: Option<McpToolDispatchGroup>,
    pub(super) executable_binding: Option<&'static ExecutableBindingV1>,
}

fn dispatch_catalog_bindings()
-> Result<Vec<DispatchCatalogBinding>, super::dispatch::McpDispatchMetadataError> {
    let mut bindings = MCP_TOOL_BINDINGS
        .iter()
        .filter(|binding| !super::handlers::INTERNAL_DAEMON_TOOL_NAMES.contains(&binding.name))
        .map(|binding| DispatchCatalogBinding {
            name: binding.name.to_owned(),
            group: binding.group,
            executable_binding: None,
        })
        .collect::<Vec<_>>();
    bindings.extend(work::dispatch_catalog_bindings()?);
    bindings.extend(workflow::dispatch_catalog_bindings()?);
    Ok(bindings)
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

pub(crate) fn canonical_tool_dispatch_ceiling(
    tool_name: &str,
) -> Result<std::time::Duration, super::dispatch::McpDispatchMetadataError> {
    let catalog = mcp_dispatch_catalog()?;
    if let Some(contract) = catalog.contract(tool_name) {
        return Ok(std::time::Duration::from_millis(
            contract.deadline().maximum_millis(),
        ));
    }
    if let Some(capability) = multi_root_capability_for_tool(tool_name)? {
        return Ok(std::time::Duration::from_millis(
            capability.deadline().maximum_millis(),
        ));
    }
    if let Some(binding) = work_executable_binding_for_tool(tool_name)? {
        return Ok(std::time::Duration::from_millis(
            binding.deadline().maximum_millis(),
        ));
    }
    if let Some(binding) = workflow_executable_binding_for_tool(tool_name)? {
        return Ok(std::time::Duration::from_millis(
            binding.deadline().maximum_millis(),
        ));
    }
    Ok(application_capability_for_tool(tool_name)?.map_or_else(
        || super::handlers::tool_dispatch_ceiling(tool_name),
        |capability| std::time::Duration::from_millis(capability.deadline().maximum_millis()),
    ))
}

/// Per-tool dispatch predicate answers, resolved once per process.
///
/// The three predicates below run several times per dispatched request
/// (connection loop, routing, dispatch controls), and each uncached
/// evaluation linearly scans the application catalog
/// (`application_capability_for_tool`). Their inputs — the static binding
/// table, the application-surface catalog, and the Work/Workflow executable
/// registries — are process-stable, so the answers are precomputed for every
/// cataloged name. The dispatch-contract cancellation/effect metadata is not
/// a substitute: it folds in workflow bindings and per-capability contracts
/// that deliberately diverge from these predicates.
#[derive(Clone, Copy)]
struct ToolDispatchPredicateFlags {
    source_edit_effect: bool,
    live_cancellation: bool,
    canonical_effect_settlement: bool,
}

fn compute_tool_dispatch_predicate_flags(tool_name: &str) -> ToolDispatchPredicateFlags {
    ToolDispatchPredicateFlags {
        source_edit_effect: compute_tool_dispatches_source_edit_effect(tool_name),
        live_cancellation: compute_tool_supports_live_cancellation(tool_name),
        canonical_effect_settlement: compute_tool_requires_canonical_effect_settlement(tool_name),
    }
}

fn tool_dispatch_predicate_flags(tool_name: &str) -> ToolDispatchPredicateFlags {
    static FLAGS: LazyLock<HashMap<String, ToolDispatchPredicateFlags>> = LazyLock::new(|| {
        let mut flags = HashMap::new();
        // A catalog enumeration failure leaves the map empty; every lookup
        // then falls back to the direct computation below, which answers
        // exactly as the uncached predicates did.
        if let Ok(bindings) = dispatch_catalog_bindings() {
            for binding in bindings {
                let per_tool = compute_tool_dispatch_predicate_flags(&binding.name);
                flags.insert(binding.name, per_tool);
            }
        }
        // Internal daemon tools are filtered out of the dispatch catalog but
        // still reach these predicates through ordinary dispatch.
        for binding in MCP_TOOL_BINDINGS.iter() {
            flags
                .entry(binding.name.to_owned())
                .or_insert_with(|| compute_tool_dispatch_predicate_flags(binding.name));
        }
        flags
    });
    FLAGS
        .get(tool_name)
        .copied()
        .unwrap_or_else(|| compute_tool_dispatch_predicate_flags(tool_name))
}

pub(crate) fn tool_dispatches_source_edit_effect(tool_name: &str) -> bool {
    tool_dispatch_predicate_flags(tool_name).source_edit_effect
}

fn compute_tool_dispatches_source_edit_effect(tool_name: &str) -> bool {
    matches!(
        binding(tool_name).and_then(|binding| binding.group),
        Some(McpToolDispatchGroup::Edit)
    ) && application_capability_for_tool(tool_name)
        .ok()
        .flatten()
        .is_some_and(|capability| capability.effect() == EffectClass::SourceEdit)
}

pub(crate) fn tool_supports_live_cancellation(tool_name: &str) -> bool {
    tool_dispatch_predicate_flags(tool_name).live_cancellation
}

fn compute_tool_supports_live_cancellation(tool_name: &str) -> bool {
    work_executable_binding_for_tool(tool_name)
        .ok()
        .flatten()
        .is_some_and(|binding| {
            matches!(
                binding.cancellation(),
                CancellationContract::Cooperative { .. }
            )
        })
        || application_capability_for_tool(tool_name)
            .ok()
            .flatten()
            .is_some_and(|capability| {
                matches!(
                    capability.cancellation(),
                    CancellationContract::Cooperative { .. }
                )
            })
        || multi_root_operation_for_tool(tool_name).is_some()
        || compute_tool_dispatches_source_edit_effect(tool_name)
        || matches!(
            tool_name,
            "tracedecay_admin_cli"
                | "tracedecay_search"
                | "tracedecay_grep"
                | "tracedecay_run_affected_tests"
                | "tracedecay_pr_context"
                | "tracedecay_dead_code"
                | "tracedecay_circular"
                | "tracedecay_affected"
                | "tracedecay_simplify_scan"
                | "tracedecay_dependency_depth"
                | "tracedecay_health"
                | "tracedecay_dsm"
        )
}

pub(crate) fn tool_requires_canonical_effect_settlement(tool_name: &str) -> bool {
    tool_dispatch_predicate_flags(tool_name).canonical_effect_settlement
}

fn compute_tool_requires_canonical_effect_settlement(tool_name: &str) -> bool {
    work_executable_binding_for_tool(tool_name)
        .ok()
        .flatten()
        .is_some_and(|binding| {
            binding.effect() != EffectClass::Read
                && *binding.cancellation() == CancellationContract::NotCancellable
        })
        || application_capability_for_tool(tool_name)
            .ok()
            .flatten()
            .is_some_and(|capability| {
                capability.effect() != EffectClass::Read
                    && *capability.cancellation() == CancellationContract::NotCancellable
            })
}

fn verified_effect_journey(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "tracedecay_dashboard"
            | "tracedecay_fact_store_curate"
            | "tracedecay_fact_store_add"
            | "tracedecay_fact_store_update"
            | "tracedecay_fact_store_remove"
            | "tracedecay_fact_store_supersede"
            | "tracedecay_fact_feedback"
            | "tracedecay_session_refresh"
            | "tracedecay_session_refresh_begin"
            | "tracedecay_session_refresh_cancel"
            | "tracedecay_run_affected_tests"
            | "tracedecay_configuration_set"
            | "tracedecay_configuration_unset"
            | "tracedecay_configuration_batch"
            | "tracedecay_configuration_write_credential"
            | "tracedecay_configuration_protected_apply"
            | "tracedecay_configuration_rollback_apply"
            | "tracedecay_context_scout_pause"
            | "tracedecay_context_scout_resume"
            | "tracedecay_str_replace"
            | "tracedecay_multi_str_replace"
            | "tracedecay_insert_at"
            | "tracedecay_ast_grep_rewrite"
            | "tracedecay_replace_symbol"
            | "tracedecay_insert_at_symbol"
            | "tracedecay_move_symbol"
            | "tracedecay_rename_symbol"
    )
}

fn executable_handler_is_available(
    tool_name: &str,
    group: Option<McpToolDispatchGroup>,
    effect: EffectClass,
    application_capability: Option<&tracedecay_tool_catalog::CapabilityManifestV1>,
) -> bool {
    matches!(
        group,
        Some(
            McpToolDispatchGroup::MultiRoot
                | McpToolDispatchGroup::Work
                | McpToolDispatchGroup::Workflow
        )
    ) || effect.is_read_only()
        || verified_effect_journey(tool_name)
        || matches!(group, Some(McpToolDispatchGroup::Edit))
            && application_capability.is_some_and(|capability| {
                capability.effect() == EffectClass::SourceEdit
                    && capability.availability().is_callable()
            })
}

fn inverse_for_tool(tool_name: &str, effect: EffectClass) -> McpInverseContract {
    if effect.is_read_only() {
        McpInverseContract::NotApplicable
    } else {
        match tool_name {
            "tracedecay_dashboard" => McpInverseContract::SameTool {
                action: "stop".to_owned(),
            },
            _ => McpInverseContract::Unavailable {
                reason: McpInverseUnavailableReason::NoVerifiedInverse,
            },
        }
    }
}

fn idempotency_for_tool(
    tool_name: &str,
    application_capability: Option<&tracedecay_tool_catalog::CapabilityManifestV1>,
) -> McpIdempotencyContract {
    match application_capability.map(tracedecay_tool_catalog::CapabilityManifestV1::idempotency) {
        Some(tracedecay_tool_catalog::IdempotencyContract::Required) => {
            McpIdempotencyContract::KeyRequired
        }
        _ if matches!(
            tool_name,
            "tracedecay_dashboard" | "tracedecay_lcm_compress" | "tracedecay_lcm_session_boundary"
        ) =>
        {
            McpIdempotencyContract::Idempotent
        }
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
    for binding in dispatch_catalog_bindings()? {
        let application_capability = application_capability_for_tool(&binding.name)?;
        let multi_root_capability = multi_root_capability_for_tool(&binding.name)?;
        let executable_binding = binding.executable_binding;
        // The multi-root catalog is the sole contract authority for its own
        // tools; every field below prefers it and falls back to the
        // application-surface capability for everything else.
        let contract_capability = multi_root_capability.as_ref().or(application_capability);
        let direct_effect = direct_effect(&binding.name);
        let effect = if direct_effect.is_effect() {
            direct_effect
        } else {
            executable_binding.map_or_else(
                || {
                    contract_capability.map_or(
                        EffectClass::Read,
                        tracedecay_tool_catalog::CapabilityManifestV1::effect,
                    )
                },
                ExecutableBindingV1::effect,
            )
        };
        let available = multi_root_capability
            .as_ref()
            .is_some_and(|capability| capability.availability().is_callable())
            || executable_binding.is_some()
            || executable_handler_is_available(
                &binding.name,
                binding.group,
                effect,
                application_capability,
            );
        let cancellation = match (multi_root_capability.as_ref(), executable_binding) {
            (Some(capability), _) => capability.cancellation().clone(),
            (None, Some(binding)) => binding.cancellation().clone(),
            (None, None) => cancellation_for_tool(&binding.name, application_capability)?,
        };
        let mut terminal_states = vec![
            McpTerminalState::Completed,
            McpTerminalState::DeadlineExceeded,
            McpTerminalState::Denied,
            McpTerminalState::Failed,
            McpTerminalState::Unavailable,
        ];
        if matches!(cancellation, CancellationContract::Cooperative { .. }) {
            terminal_states.push(McpTerminalState::Cancelled);
        }
        let streaming = contract_capability
            .map(tracedecay_tool_catalog::CapabilityManifestV1::streaming)
            .filter(|streaming| streaming.is_supported())
            .cloned();
        contracts.push(McpDispatchContractV1::new(McpDispatchContractInputV1 {
            tool_name: binding.name.clone(),
            availability: if available {
                McpDispatchAvailability::Available
            } else {
                McpDispatchAvailability::Unavailable {
                    reason: McpDispatchUnavailableReason::EffectJourneyUnverified,
                    retryable: false,
                }
            },
            effect,
            deadline: McpDeadlineContractV1::new(executable_binding.map_or_else(
                || {
                    contract_capability.map_or_else(
                        || super::handlers::tool_dispatch_ceiling(&binding.name).as_millis() as u64,
                        |capability| capability.deadline().maximum_millis(),
                    )
                },
                |binding| binding.deadline().maximum_millis(),
            ))?,
            idempotency: multi_root_capability.as_ref().map_or_else(
                || {
                    executable_binding.map_or_else(
                        || idempotency_for_tool(&binding.name, application_capability),
                        |binding| match binding.idempotency() {
                            tracedecay_tool_catalog::IdempotencyContract::Required => {
                                McpIdempotencyContract::KeyRequired
                            }
                            tracedecay_tool_catalog::IdempotencyContract::NotRequired => {
                                McpIdempotencyContract::NotProvided
                            }
                        },
                    )
                },
                |capability| match capability.idempotency() {
                    tracedecay_tool_catalog::IdempotencyContract::Required => {
                        McpIdempotencyContract::KeyRequired
                    }
                    tracedecay_tool_catalog::IdempotencyContract::NotRequired => {
                        McpIdempotencyContract::NotProvided
                    }
                },
            ),
            inverse: inverse_for_tool(&binding.name, effect),
            cancellation,
            terminal_states,
            pagination: contract_capability
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
#[cfg(test)]
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
    use tracedecay_tool_catalog::{ApplicationSurfaceOperation, ProfileId};

    use super::*;

    #[test]
    fn every_tool_is_bound_once() {
        let mut names = dispatch_catalog_bindings()
            .unwrap()
            .into_iter()
            .map(|entry| entry.name)
            .collect::<Vec<_>>();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "a tool name is bound twice");
    }

    #[test]
    fn derived_reader_rows_match_mcp_catalog() {
        let _ = &*MCP_TOOL_BINDINGS;
        let mut from_table = registered_project_reader_tool_names();
        let mut from_mcp = tracedecay_mcp::registered_project_reader_tool_names();
        from_table.sort_unstable();
        from_mcp.sort_unstable();
        assert_eq!(
            from_table, from_mcp,
            "derived Reader rows must come from the tracedecay-mcp catalog"
        );
        assert!(
            !from_mcp.is_empty(),
            "mcp reader catalog must advertise at least one Reader tool"
        );
    }

    #[test]
    fn dispatch_catalog_covers_every_advertised_binding_with_canonical_deadline() {
        let catalog = mcp_dispatch_catalog().unwrap();
        for contract in catalog.contracts() {
            let application_capability =
                application_capability_for_tool(contract.tool_name()).unwrap();
            assert_eq!(
                contract.deadline().maximum_millis(),
                canonical_tool_dispatch_ceiling(contract.tool_name())
                    .unwrap()
                    .as_millis() as u64,
                "{} must use one canonical dispatch ceiling",
                contract.tool_name(),
            );
            assert_eq!(
                contract.availability().is_available(),
                executable_handler_is_available(
                    contract.tool_name(),
                    dispatch_group_for_tool(contract.tool_name()),
                    contract.effect(),
                    application_capability,
                )
            );
        }
    }

    #[test]
    fn memory_status_is_a_read_only_retained_operation() {
        let catalog = mcp_dispatch_catalog().unwrap();
        let contract = catalog.contract("tracedecay_memory_status").unwrap();
        assert_eq!(contract.effect(), EffectClass::Read);
        assert!(contract.read_only());
        assert!(contract.availability().is_available());
        assert_eq!(contract.idempotency(), McpIdempotencyContract::NotProvided);
        assert!(matches!(
            contract.inverse(),
            McpInverseContract::NotApplicable
        ));
    }

    #[test]
    fn lcm_doctor_is_a_read_only_diagnostic() {
        let contract = mcp_dispatch_catalog()
            .unwrap()
            .contract("tracedecay_lcm_doctor")
            .unwrap();
        assert_eq!(contract.effect(), EffectClass::Read);
        assert!(contract.read_only());
        assert!(contract.availability().is_available());
        assert_eq!(contract.idempotency(), McpIdempotencyContract::NotProvided);
        assert!(matches!(
            contract.inverse(),
            McpInverseContract::NotApplicable
        ));
    }

    #[test]
    fn source_edit_reconcile_is_available_with_the_daemon_owned_recovery_path() {
        let catalog = mcp_dispatch_catalog().unwrap();
        assert!(
            catalog
                .contract("tracedecay_source_edit_reconcile")
                .unwrap()
                .availability()
                .is_available()
        );
    }

    #[test]
    fn configuration_effects_are_available_after_their_canonical_journeys_ship() {
        let catalog = mcp_dispatch_catalog().unwrap();
        for tool_name in [
            "tracedecay_configuration_set",
            "tracedecay_configuration_unset",
            "tracedecay_configuration_batch",
            "tracedecay_configuration_write_credential",
            "tracedecay_configuration_protected_apply",
            "tracedecay_configuration_rollback_apply",
        ] {
            let contract = catalog.contract(tool_name).unwrap();
            assert_eq!(contract.effect(), EffectClass::ConfigurationWrite);
            assert_eq!(contract.deadline().maximum_millis(), 15_000);
            assert!(contract.availability().is_available());
            assert_eq!(contract.idempotency(), McpIdempotencyContract::KeyRequired);
            assert!(matches!(
                contract.cancellation(),
                CancellationContract::NotCancellable
            ));
        }
    }

    #[test]
    fn retained_administrative_effects_are_available_after_their_canonical_journeys_ship() {
        let catalog = mcp_dispatch_catalog().unwrap();
        for tool_name in [
            "tracedecay_fact_store_curate",
            "tracedecay_fact_feedback",
            "tracedecay_session_refresh",
            "tracedecay_run_affected_tests",
        ] {
            let contract = catalog.contract(tool_name).unwrap();
            assert_eq!(contract.effect(), EffectClass::Administrative);
            assert!(
                contract.availability().is_available(),
                "{tool_name} must stay callable once its retained MCP journey ships"
            );
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
        let diagnostics = catalog.contract("tracedecay_diagnostics").unwrap();
        assert!(matches!(
            diagnostics.cancellation(),
            CancellationContract::NotCancellable
        ));
        assert!(
            !diagnostics
                .terminal_states()
                .contains(&McpTerminalState::Cancelled),
            "terminal states follow the resolved contract, not the broad dispatch predicate"
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
    fn remote_status_is_an_active_project_info_read() {
        let entry = MCP_TOOL_BINDINGS
            .iter()
            .find(|entry| entry.name == "tracedecay_remote_status")
            .expect("tracedecay_remote_status must have a binding row");
        assert_eq!(entry.group, Some(McpToolDispatchGroup::Info));
        assert_eq!(entry.project, RegisteredProjectAccess::ActiveProjectOnly);
        assert!(!tool_accepts_registered_project_selector(entry.name));
        assert!(!tool_dispatches_registered_project_reader(entry.name));
    }

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

    #[test]
    fn exact_fact_routes_accept_selectors_without_registered_reader_dispatch() {
        for tool_name in [
            "tracedecay_fact_store_add",
            "tracedecay_fact_store_search",
            "tracedecay_fact_store_probe",
            "tracedecay_fact_store_related",
            "tracedecay_fact_store_reason",
            "tracedecay_fact_store_contradict",
            "tracedecay_fact_store_get",
            "tracedecay_fact_store_update",
            "tracedecay_fact_store_remove",
            "tracedecay_fact_store_supersede",
            "tracedecay_fact_store_list",
            "tracedecay_memory_status",
        ] {
            assert!(tool_accepts_registered_project_selector(tool_name));
            assert!(!tool_dispatches_registered_project_reader(tool_name));
        }
        for tool_name in [
            "tracedecay_fact_store_add",
            "tracedecay_fact_store_update",
            "tracedecay_fact_store_remove",
            "tracedecay_fact_store_supersede",
        ] {
            assert!(
                tool_is_selector_bound_effect(tool_name),
                "{tool_name} must stay selector-bound so writes are not dispatched into the selected store"
            );
        }
        for tool_name in [
            "tracedecay_fact_store_search",
            "tracedecay_fact_store_get",
            "tracedecay_memory_status",
        ] {
            assert!(
                !tool_is_selector_bound_effect(tool_name),
                "{tool_name} is a selector-bound read and must keep its existing selected-project route"
            );
        }
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
                || RetainedSurfaceOperation::from_tool_name(entry.name).is_some();
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
                RetainedSurfaceOperation::from_tool_name(entry.name).is_none(),
                "{} would change groups under a flat lookup",
                entry.name
            );
        }
    }

    /// Every Independent catalog tool, plus one Sensitive representative per
    /// dispatch family that still reopens on branch drift. An advertised tool
    /// missing from this table must stay Sensitive (fail-safe).
    #[rustfmt::skip]
    const PINNED_BRANCH_SENSITIVITY: &[(&str, BranchSensitivity)] = &[
        // Memory: ledger / skill / analytics reads. Handlers use project_root,
        // store_layout, and memory identity — never the code graph.
        ("tracedecay_automation_run_list", BranchSensitivity::Independent),
        ("tracedecay_automation_run_view", BranchSensitivity::Independent),
        ("tracedecay_automation_run_artifact_view", BranchSensitivity::Independent),
        ("tracedecay_analytics", BranchSensitivity::Independent),
        ("tracedecay_skill_list", BranchSensitivity::Independent),
        ("tracedecay_skill_view", BranchSensitivity::Independent),
        ("tracedecay_hermes_skill_bridge", BranchSensitivity::Independent),
        // Work: product-graph / attempt lifecycle via the Work daemon owner.
        // `handle_work` does not take `cg`; MutateGraph writes the Work
        // product graph, not the code index.
        ("tracedecay_work_generate_proposal", BranchSensitivity::Independent),
        ("tracedecay_work_create", BranchSensitivity::Independent),
        ("tracedecay_work_review_proposal", BranchSensitivity::Independent),
        ("tracedecay_work_accept_proposal", BranchSensitivity::Independent),
        ("tracedecay_work_admit_execution", BranchSensitivity::Independent),
        ("tracedecay_work_start_attempt", BranchSensitivity::Independent),
        ("tracedecay_work_synthesize", BranchSensitivity::Independent),
        ("tracedecay_work_attempt_status", BranchSensitivity::Independent),
        ("tracedecay_work_cancel_attempt", BranchSensitivity::Independent),
        ("tracedecay_work_resume_attempts", BranchSensitivity::Independent),
        ("tracedecay_work_retry_attempt", BranchSensitivity::Independent),
        ("tracedecay_work_list_attempts", BranchSensitivity::Independent),
        ("tracedecay_work_execution_history", BranchSensitivity::Independent),
        ("tracedecay_work_hydrate_artifacts", BranchSensitivity::Independent),
        ("tracedecay_work_retrieve_evidence", BranchSensitivity::Independent),
        ("tracedecay_work_views", BranchSensitivity::Independent),
        ("tracedecay_work_experience", BranchSensitivity::Independent),
        ("tracedecay_work_compare_proposal", BranchSensitivity::Independent),
        ("tracedecay_work_prepare_graph_mutation", BranchSensitivity::Independent),
        ("tracedecay_work_mutate_graph", BranchSensitivity::Independent),
        ("tracedecay_work_topology", BranchSensitivity::Independent),
        ("tracedecay_work_topology_metrics", BranchSensitivity::Independent),
        ("tracedecay_work_prepare_duplicate_adjudication", BranchSensitivity::Independent),
        ("tracedecay_work_adjudicate_duplicate", BranchSensitivity::Independent),
        ("tracedecay_work_adjudicate_leak", BranchSensitivity::Independent),
        ("tracedecay_work_pause_run", BranchSensitivity::Independent),
        ("tracedecay_work_resume_run", BranchSensitivity::Independent),
        ("tracedecay_work_run_control", BranchSensitivity::Independent),
        ("tracedecay_work_placement_preflight", BranchSensitivity::Independent),
        ("tracedecay_work_admit_placement", BranchSensitivity::Independent),
        ("tracedecay_work_placement_status", BranchSensitivity::Independent),
        ("tracedecay_work_release_placement", BranchSensitivity::Independent),
        // Workflow: definition and run lifecycle via the Workflow daemon owner.
        // `handle_workflow` does not take `cg`.
        ("tracedecay_workflow_register_definition", BranchSensitivity::Independent),
        ("tracedecay_workflow_activate_definition", BranchSensitivity::Independent),
        ("tracedecay_workflow_retire_definition", BranchSensitivity::Independent),
        ("tracedecay_workflow_reject_definition", BranchSensitivity::Independent),
        ("tracedecay_workflow_validate_definition", BranchSensitivity::Independent),
        ("tracedecay_workflow_get_definition", BranchSensitivity::Independent),
        ("tracedecay_workflow_list_definitions", BranchSensitivity::Independent),
        ("tracedecay_workflow_definition_history", BranchSensitivity::Independent),
        ("tracedecay_workflow_diff_definition", BranchSensitivity::Independent),
        ("tracedecay_workflow_handoff_issue", BranchSensitivity::Independent),
        ("tracedecay_workflow_handoff_redeem", BranchSensitivity::Independent),
        ("tracedecay_workflow_start_run", BranchSensitivity::Independent),
        ("tracedecay_workflow_pause_run", BranchSensitivity::Independent),
        ("tracedecay_workflow_resume_run", BranchSensitivity::Independent),
        ("tracedecay_workflow_cancel_run", BranchSensitivity::Independent),
        ("tracedecay_workflow_get_run", BranchSensitivity::Independent),
        // Retained surface: fact-store, LCM, and session memory. Dispatch
        // renders with `cg.project_root()` only.
        ("tracedecay_fact_store_curate", BranchSensitivity::Independent),
        ("tracedecay_fact_store_add", BranchSensitivity::Independent),
        ("tracedecay_fact_store_search", BranchSensitivity::Independent),
        ("tracedecay_fact_store_probe", BranchSensitivity::Independent),
        ("tracedecay_fact_store_related", BranchSensitivity::Independent),
        ("tracedecay_fact_store_reason", BranchSensitivity::Independent),
        ("tracedecay_fact_store_contradict", BranchSensitivity::Independent),
        ("tracedecay_fact_store_get", BranchSensitivity::Independent),
        ("tracedecay_fact_store_update", BranchSensitivity::Independent),
        ("tracedecay_fact_store_remove", BranchSensitivity::Independent),
        ("tracedecay_fact_store_list", BranchSensitivity::Independent),
        ("tracedecay_fact_store_supersede", BranchSensitivity::Independent),
        ("tracedecay_fact_feedback", BranchSensitivity::Independent),
        ("tracedecay_memory_status", BranchSensitivity::Independent),
        ("tracedecay_session_refresh", BranchSensitivity::Independent),
        ("tracedecay_session_refresh_status", BranchSensitivity::Independent),
        ("tracedecay_session_refresh_cancel", BranchSensitivity::Independent),
        ("tracedecay_session_refresh_begin", BranchSensitivity::Independent),
        ("tracedecay_message_search", BranchSensitivity::Independent),
        ("tracedecay_sessions_for", BranchSensitivity::Independent),
        ("tracedecay_workflows", BranchSensitivity::Independent),
        ("tracedecay_lcm_status", BranchSensitivity::Independent),
        ("tracedecay_lcm_doctor", BranchSensitivity::Independent),
        ("tracedecay_lcm_load_session", BranchSensitivity::Independent),
        ("tracedecay_lcm_grep", BranchSensitivity::Independent),
        ("tracedecay_lcm_describe", BranchSensitivity::Independent),
        ("tracedecay_lcm_expand", BranchSensitivity::Independent),
        ("tracedecay_lcm_expand_query", BranchSensitivity::Independent),
        // ApplicationSurface Independent: configuration, scout lifecycle,
        // session identity, store status, observatory, host-integration
        // apply. MCP render uses `cg.project_root()` only; owners do not
        // read the code graph.
        ("tracedecay_configuration_list", BranchSensitivity::Independent),
        ("tracedecay_configuration_explain", BranchSensitivity::Independent),
        ("tracedecay_configuration_get", BranchSensitivity::Independent),
        ("tracedecay_configuration_set", BranchSensitivity::Independent),
        ("tracedecay_configuration_unset", BranchSensitivity::Independent),
        ("tracedecay_configuration_batch", BranchSensitivity::Independent),
        ("tracedecay_configuration_write_credential", BranchSensitivity::Independent),
        ("tracedecay_configuration_observed_state", BranchSensitivity::Independent),
        ("tracedecay_configuration_protected_preview", BranchSensitivity::Independent),
        ("tracedecay_configuration_protected_apply", BranchSensitivity::Independent),
        ("tracedecay_configuration_rollback_preview", BranchSensitivity::Independent),
        ("tracedecay_configuration_rollback_apply", BranchSensitivity::Independent),
        ("tracedecay_configuration_audit", BranchSensitivity::Independent),
        ("tracedecay_context_scout_status", BranchSensitivity::Independent),
        ("tracedecay_context_scout_recent", BranchSensitivity::Independent),
        ("tracedecay_context_scout_explain", BranchSensitivity::Independent),
        ("tracedecay_context_scout_capability", BranchSensitivity::Independent),
        ("tracedecay_context_scout_budget", BranchSensitivity::Independent),
        ("tracedecay_context_scout_pause", BranchSensitivity::Independent),
        ("tracedecay_context_scout_resume", BranchSensitivity::Independent),
        ("tracedecay_context_scout_cancel", BranchSensitivity::Independent),
        ("tracedecay_context_scout_claim", BranchSensitivity::Independent),
        ("tracedecay_context_scout_delivery", BranchSensitivity::Independent),
        ("tracedecay_context_scout_feedback", BranchSensitivity::Independent),
        ("tracedecay_session_lookup", BranchSensitivity::Independent),
        ("tracedecay_storage_status", BranchSensitivity::Independent),
        ("tracedecay_observatory_read", BranchSensitivity::Independent),
        ("tracedecay_preflight_native_integration", BranchSensitivity::Independent),
        ("tracedecay_approve_native_integration", BranchSensitivity::Independent),
        ("tracedecay_apply_native_integration", BranchSensitivity::Independent),
        ("tracedecay_native_integration_status", BranchSensitivity::Independent),
        ("tracedecay_cancel_native_integration", BranchSensitivity::Independent),
        // One Sensitive representative per remaining dispatch family.
        ("tracedecay_search", BranchSensitivity::Sensitive),
        ("tracedecay_status", BranchSensitivity::Sensitive),
        ("tracedecay_admin_cli", BranchSensitivity::Sensitive),
        ("tracedecay_dead_code", BranchSensitivity::Sensitive),
        ("tracedecay_affected", BranchSensitivity::Sensitive),
        ("tracedecay_str_replace", BranchSensitivity::Sensitive),
        ("tracedecay_health", BranchSensitivity::Sensitive),
        ("tracedecay_multi_root_execute", BranchSensitivity::Sensitive),
        ("tracedecay_code_symbol_search", BranchSensitivity::Sensitive),
        ("tracedecay_diagnose", BranchSensitivity::Sensitive),
        ("tracedecay_run_affected_tests", BranchSensitivity::Sensitive),
        ("tracedecay_dashboard", BranchSensitivity::Sensitive),
    ];

    fn advertised_tool_names(mode: tracedecay_mcp::ToolRegistryMode) -> Vec<String> {
        use crate::mcp::tools::catalog_discovery::{
            default_catalog_discovery_authority, get_catalog_filtered_tool_definitions_with_budget,
        };
        use tracedecay_mcp::{explore_call_budget, project_catalog_discovery_scope};

        let profile_id = ProfileId::new(tracedecay_application::APPLICATION_DEFAULT_PROFILE_ID)
            .expect("default profile");
        get_catalog_filtered_tool_definitions_with_budget(
            0,
            explore_call_budget(0),
            &profile_id,
            &default_catalog_discovery_authority().expect("default discovery authority"),
            &project_catalog_discovery_scope(),
            mode,
        )
        .expect("advertised tools")
        .into_iter()
        .map(|definition| definition.name)
        .collect()
    }

    /// Every advertised tool resolves to one policy without panicking. The
    /// pinned table is the Independent allowlist plus one Sensitive
    /// representative per remaining family; an unlisted advertised Independent
    /// classification is a failed audit, not a silent pass.
    #[test]
    fn advertised_tools_have_exactly_one_branch_sensitivity_policy() {
        use std::collections::HashMap;
        use tracedecay_mcp::ToolRegistryMode;

        let pinned: HashMap<&str, BranchSensitivity> =
            PINNED_BRANCH_SENSITIVITY.iter().copied().collect();
        for (name, expected) in PINNED_BRANCH_SENSITIVITY {
            assert_eq!(
                tool_branch_sensitivity(name),
                *expected,
                "{name} must keep its pinned branch-sensitivity"
            );
        }
        let mut policies = HashMap::new();
        let mut advertised = 0usize;
        for mode in [
            ToolRegistryMode::DeterministicMaximal,
            ToolRegistryMode::HostAvailable,
        ] {
            for name in advertised_tool_names(mode) {
                advertised += 1;
                let policy = tool_branch_sensitivity(&name);
                assert!(
                    matches!(
                        policy,
                        BranchSensitivity::Sensitive | BranchSensitivity::Independent
                    ),
                    "{name} must resolve to exactly one sensitivity policy"
                );
                if let Some(previous) = policies.insert(name.clone(), policy) {
                    assert_eq!(
                        previous, policy,
                        "{name} must not change policy across registry modes"
                    );
                }
                if let Some(expected) = pinned.get(name.as_str()) {
                    assert_eq!(
                        policy, *expected,
                        "{name} must keep its pinned branch-sensitivity"
                    );
                } else {
                    assert_eq!(
                        policy,
                        BranchSensitivity::Sensitive,
                        "{name} is not in the Independent pin and must stay Sensitive"
                    );
                }
            }
        }
        assert!(
            advertised > 0,
            "tools/list must advertise at least one tool in each registry mode"
        );
        assert!(
            policies
                .values()
                .any(|policy| *policy == BranchSensitivity::Independent),
            "the advertised set must include the pinned Independent tools"
        );
        assert!(
            policies
                .values()
                .any(|policy| *policy == BranchSensitivity::Sensitive),
            "the advertised set must include branch-sensitive graph/git/edit tools"
        );
    }
}
