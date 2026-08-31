use serde::{Deserialize, Serialize};

/// Canonical operation identity shared by every retained application surface.
///
/// Transport bindings select the exposed subset without defining another
/// operation enum or name conversion.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationSurfaceOperation {
    GitStatus,
    GitDiff,
    GitHistory,
    GitBlame,
    GitHunks,
    GitPreview,
    GitApply,
    GitHubStackSignalExpand,
    NativeIntegrationStackSnapshot,
    NativeIntegrationPreflight,
    NativeIntegrationApprove,
    NativeIntegrationApply,
    NativeIntegrationStatus,
    NativeIntegrationCancel,
    NativeIntegrationWorktreeInventory,
    NativeIntegrationWorktreeInspect,
    NativeIntegrationWorktreeConfirm,
    NativeIntegrationWorktreeRemove,
    NativeIntegrationWorktreeReconcile,
    FeedbackDiagnostics,
    FeedbackGet,
    FeedbackExpand,
    FeedbackList,
    FeedbackImpact,
    FeedbackAdvisoryCycle,
    AffectedTests,
    TestResults,
    CodeExactOccurrence,
    CodePhraseSearch,
    CodeSymbolSearch,
    CodeSignatureSearch,
    CodeImplementations,
    CodeTypeHierarchy,
    CodeCallers,
    CodeCallees,
    CodeFacets,
    CodeTimeline,
    CodeDeclaration,
    CodeDefinition,
    CodeTypeDefinition,
    CodeReferences,
    SessionLookup,
    QualifiedName,
    CallChain,
    FileDependents,
    SourceLines,
    SourceBody,
    SourceOutline,
    ModuleApi,
    FileMetadata,
    HealthRead,
    HealthDelta,
    StorageStatus,
    DiagnosticsRead,
    ObservatoryRead,
    ConfigurationList,
    ConfigurationExplain,
    ConfigurationGet,
    ConfigurationSet,
    ConfigurationUnset,
    ConfigurationBatch,
    ConfigurationWriteCredential,
    ConfigurationObservedState,
    ConfigurationProtectedPreview,
    ConfigurationProtectedApply,
    ConfigurationRollbackPreview,
    ConfigurationRollbackApply,
    ConfigurationAudit,
    ContextScoutStatus,
    ContextScoutRecent,
    ContextScoutExplain,
    ContextScoutCapability,
    ContextScoutBudget,
    ContextScoutPause,
    ContextScoutResume,
    ContextScoutCancel,
    ContextScoutClaim,
    ContextScoutDelivery,
    ContextScoutFeedback,
}

impl ApplicationSurfaceOperation {
    pub const ALL: [Self; 79] = [
        Self::GitStatus,
        Self::GitDiff,
        Self::GitHistory,
        Self::GitBlame,
        Self::GitHunks,
        Self::GitPreview,
        Self::GitApply,
        Self::GitHubStackSignalExpand,
        Self::NativeIntegrationStackSnapshot,
        Self::NativeIntegrationPreflight,
        Self::NativeIntegrationApprove,
        Self::NativeIntegrationApply,
        Self::NativeIntegrationStatus,
        Self::NativeIntegrationCancel,
        Self::NativeIntegrationWorktreeInventory,
        Self::NativeIntegrationWorktreeInspect,
        Self::NativeIntegrationWorktreeConfirm,
        Self::NativeIntegrationWorktreeRemove,
        Self::NativeIntegrationWorktreeReconcile,
        Self::FeedbackDiagnostics,
        Self::FeedbackGet,
        Self::FeedbackExpand,
        Self::FeedbackList,
        Self::FeedbackImpact,
        Self::FeedbackAdvisoryCycle,
        Self::AffectedTests,
        Self::TestResults,
        Self::CodeExactOccurrence,
        Self::CodePhraseSearch,
        Self::CodeSymbolSearch,
        Self::CodeSignatureSearch,
        Self::CodeImplementations,
        Self::CodeTypeHierarchy,
        Self::CodeCallers,
        Self::CodeCallees,
        Self::CodeFacets,
        Self::CodeTimeline,
        Self::CodeDeclaration,
        Self::CodeDefinition,
        Self::CodeTypeDefinition,
        Self::CodeReferences,
        Self::SessionLookup,
        Self::QualifiedName,
        Self::CallChain,
        Self::FileDependents,
        Self::SourceLines,
        Self::SourceBody,
        Self::SourceOutline,
        Self::ModuleApi,
        Self::FileMetadata,
        Self::HealthRead,
        Self::HealthDelta,
        Self::StorageStatus,
        Self::DiagnosticsRead,
        Self::ObservatoryRead,
        Self::ConfigurationList,
        Self::ConfigurationExplain,
        Self::ConfigurationGet,
        Self::ConfigurationSet,
        Self::ConfigurationUnset,
        Self::ConfigurationBatch,
        Self::ConfigurationWriteCredential,
        Self::ConfigurationObservedState,
        Self::ConfigurationProtectedPreview,
        Self::ConfigurationProtectedApply,
        Self::ConfigurationRollbackPreview,
        Self::ConfigurationRollbackApply,
        Self::ConfigurationAudit,
        Self::ContextScoutStatus,
        Self::ContextScoutRecent,
        Self::ContextScoutExplain,
        Self::ContextScoutCapability,
        Self::ContextScoutBudget,
        Self::ContextScoutPause,
        Self::ContextScoutResume,
        Self::ContextScoutCancel,
        Self::ContextScoutClaim,
        Self::ContextScoutDelivery,
        Self::ContextScoutFeedback,
    ];

    pub fn from_catalog_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|operation| operation.as_str() == name)
    }

    pub fn from_tool_name(tool_name: &str) -> Option<Self> {
        let operation = match tool_name.strip_prefix("tracedecay_") {
            Some(operation) => operation,
            None => tool_name,
        };
        if operation == "diagnostics" {
            return Some(Self::DiagnosticsRead);
        }
        Self::from_catalog_name(operation)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GitStatus => "git_status",
            Self::GitDiff => "git_diff",
            Self::GitHistory => "git_history",
            Self::GitBlame => "git_blame",
            Self::GitHunks => "git_hunks",
            Self::GitPreview => "git_preview",
            Self::GitApply => "git_apply",
            Self::GitHubStackSignalExpand => "github_stack_signal_expand",
            Self::NativeIntegrationStackSnapshot => "stack_snapshot",
            Self::NativeIntegrationPreflight => "preflight_native_integration",
            Self::NativeIntegrationApprove => "approve_native_integration",
            Self::NativeIntegrationApply => "apply_native_integration",
            Self::NativeIntegrationStatus => "native_integration_status",
            Self::NativeIntegrationCancel => "cancel_native_integration",
            Self::NativeIntegrationWorktreeInventory => "worktree_inventory",
            Self::NativeIntegrationWorktreeInspect => "worktree_cleanup_inspect",
            Self::NativeIntegrationWorktreeConfirm => "worktree_cleanup_confirm",
            Self::NativeIntegrationWorktreeRemove => "worktree_cleanup_remove",
            Self::NativeIntegrationWorktreeReconcile => "worktree_cleanup_reconcile",
            Self::FeedbackDiagnostics => "feedback_diagnostics",
            Self::FeedbackGet => "feedback_get",
            Self::FeedbackExpand => "feedback_expand",
            Self::FeedbackList => "feedback_list",
            Self::FeedbackImpact => "feedback_impact",
            Self::FeedbackAdvisoryCycle => "feedback_advisory_cycle",
            Self::AffectedTests => "affected_tests",
            Self::TestResults => "test_results",
            Self::CodeExactOccurrence => "code_exact_occurrence",
            Self::CodePhraseSearch => "code_phrase_search",
            Self::CodeSymbolSearch => "code_symbol_search",
            Self::CodeSignatureSearch => "code_signature_search",
            Self::CodeImplementations => "code_implementations",
            Self::CodeTypeHierarchy => "code_type_hierarchy",
            Self::CodeCallers => "code_callers",
            Self::CodeCallees => "code_callees",
            Self::CodeFacets => "code_facets",
            Self::CodeTimeline => "code_timeline",
            Self::CodeDeclaration => "code_declaration",
            Self::CodeDefinition => "code_definition",
            Self::CodeTypeDefinition => "code_type_definition",
            Self::CodeReferences => "code_references",
            Self::SessionLookup => "session_lookup",
            Self::QualifiedName => "qualified_name",
            Self::CallChain => "call_chain",
            Self::FileDependents => "file_dependents",
            Self::SourceLines => "source_lines",
            Self::SourceBody => "source_body",
            Self::SourceOutline => "source_outline",
            Self::ModuleApi => "module_api",
            Self::FileMetadata => "file_metadata",
            Self::HealthRead => "health_read",
            Self::HealthDelta => "health_delta",
            Self::StorageStatus => "storage_status",
            Self::DiagnosticsRead => "diagnostics_read",
            Self::ObservatoryRead => "observatory_read",
            Self::ConfigurationList => "configuration_list",
            Self::ConfigurationExplain => "configuration_explain",
            Self::ConfigurationGet => "configuration_get",
            Self::ConfigurationSet => "configuration_set",
            Self::ConfigurationUnset => "configuration_unset",
            Self::ConfigurationBatch => "configuration_batch",
            Self::ConfigurationWriteCredential => "configuration_write_credential",
            Self::ConfigurationObservedState => "configuration_observed_state",
            Self::ConfigurationProtectedPreview => "configuration_protected_preview",
            Self::ConfigurationProtectedApply => "configuration_protected_apply",
            Self::ConfigurationRollbackPreview => "configuration_rollback_preview",
            Self::ConfigurationRollbackApply => "configuration_rollback_apply",
            Self::ConfigurationAudit => "configuration_audit",
            Self::ContextScoutStatus => "context_scout_status",
            Self::ContextScoutRecent => "context_scout_recent",
            Self::ContextScoutExplain => "context_scout_explain",
            Self::ContextScoutCapability => "context_scout_capability",
            Self::ContextScoutBudget => "context_scout_budget",
            Self::ContextScoutPause => "context_scout_pause",
            Self::ContextScoutResume => "context_scout_resume",
            Self::ContextScoutCancel => "context_scout_cancel",
            Self::ContextScoutClaim => "context_scout_claim",
            Self::ContextScoutDelivery => "context_scout_delivery",
            Self::ContextScoutFeedback => "context_scout_feedback",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ApplicationSurfaceOperation;

    const CANONICAL_NAMES: [&str; 79] = [
        "git_status",
        "git_diff",
        "git_history",
        "git_blame",
        "git_hunks",
        "git_preview",
        "git_apply",
        "github_stack_signal_expand",
        "stack_snapshot",
        "preflight_native_integration",
        "approve_native_integration",
        "apply_native_integration",
        "native_integration_status",
        "cancel_native_integration",
        "worktree_inventory",
        "worktree_cleanup_inspect",
        "worktree_cleanup_confirm",
        "worktree_cleanup_remove",
        "worktree_cleanup_reconcile",
        "feedback_diagnostics",
        "feedback_get",
        "feedback_expand",
        "feedback_list",
        "feedback_impact",
        "feedback_advisory_cycle",
        "affected_tests",
        "test_results",
        "code_exact_occurrence",
        "code_phrase_search",
        "code_symbol_search",
        "code_signature_search",
        "code_implementations",
        "code_type_hierarchy",
        "code_callers",
        "code_callees",
        "code_facets",
        "code_timeline",
        "code_declaration",
        "code_definition",
        "code_type_definition",
        "code_references",
        "session_lookup",
        "qualified_name",
        "call_chain",
        "file_dependents",
        "source_lines",
        "source_body",
        "source_outline",
        "module_api",
        "file_metadata",
        "health_read",
        "health_delta",
        "storage_status",
        "diagnostics_read",
        "observatory_read",
        "configuration_list",
        "configuration_explain",
        "configuration_get",
        "configuration_set",
        "configuration_unset",
        "configuration_batch",
        "configuration_write_credential",
        "configuration_observed_state",
        "configuration_protected_preview",
        "configuration_protected_apply",
        "configuration_rollback_preview",
        "configuration_rollback_apply",
        "configuration_audit",
        "context_scout_status",
        "context_scout_recent",
        "context_scout_explain",
        "context_scout_capability",
        "context_scout_budget",
        "context_scout_pause",
        "context_scout_resume",
        "context_scout_cancel",
        "context_scout_claim",
        "context_scout_delivery",
        "context_scout_feedback",
    ];

    #[test]
    fn canonical_names_preserve_order_and_round_trip() {
        assert_eq!(ApplicationSurfaceOperation::ALL.len(), 79);
        for (operation, expected_name) in ApplicationSurfaceOperation::ALL
            .into_iter()
            .zip(CANONICAL_NAMES)
        {
            assert_eq!(operation.as_str(), expected_name);
            assert_eq!(
                ApplicationSurfaceOperation::from_catalog_name(expected_name),
                Some(operation)
            );
        }
        assert_eq!(
            ApplicationSurfaceOperation::from_catalog_name("not_an_operation"),
            None
        );
    }

    #[test]
    fn serde_wire_names_are_exact() {
        let representatives = [
            (ApplicationSurfaceOperation::GitStatus, "\"git_status\""),
            (
                ApplicationSurfaceOperation::GitHubStackSignalExpand,
                "\"git_hub_stack_signal_expand\"",
            ),
            (
                ApplicationSurfaceOperation::NativeIntegrationStackSnapshot,
                "\"native_integration_stack_snapshot\"",
            ),
            (
                ApplicationSurfaceOperation::DiagnosticsRead,
                "\"diagnostics_read\"",
            ),
        ];

        for (operation, expected_json) in representatives {
            assert_eq!(serde_json::to_string(&operation).unwrap(), expected_json);
            assert_eq!(
                serde_json::from_str::<ApplicationSurfaceOperation>(expected_json).unwrap(),
                operation
            );
        }
    }

    #[test]
    fn tool_names_resolve_prefixes_and_diagnostics_alias() {
        for operation in ApplicationSurfaceOperation::ALL {
            assert_eq!(
                ApplicationSurfaceOperation::from_tool_name(operation.as_str()),
                Some(operation)
            );
            assert_eq!(
                ApplicationSurfaceOperation::from_tool_name(&format!(
                    "tracedecay_{}",
                    operation.as_str()
                )),
                Some(operation)
            );
        }
        assert_eq!(
            ApplicationSurfaceOperation::from_tool_name("diagnostics"),
            Some(ApplicationSurfaceOperation::DiagnosticsRead)
        );
        assert_eq!(
            ApplicationSurfaceOperation::from_tool_name("tracedecay_diagnostics"),
            Some(ApplicationSurfaceOperation::DiagnosticsRead)
        );
        assert_eq!(
            ApplicationSurfaceOperation::from_tool_name("tracedecay_not_an_operation"),
            None
        );
    }

    #[test]
    fn operation_identity_is_copy_eq_and_hash() {
        fn assert_traits<T: Copy + Eq + std::hash::Hash>() {}

        assert_traits::<ApplicationSurfaceOperation>();
    }
}
