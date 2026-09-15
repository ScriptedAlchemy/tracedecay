use serde::{Deserialize, Serialize};

macro_rules! count_operations {
    () => {
        0usize
    };
    ($head:ident $(, $tail:ident)*) => {
        1usize + count_operations!($($tail),*)
    };
}

macro_rules! application_surface_operations {
    (
        $(
            $variant:ident => $catalog_name:literal
            $(, mcp: $mcp_name:literal)?;
        )+
    ) => {
        /// Canonical operation identity shared by every retained application surface.
        ///
        /// Transport bindings select the exposed subset without defining another
        /// operation enum or name conversion.
        #[derive(
            Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash,
        )]
        #[serde(rename_all = "snake_case")]
        pub enum ApplicationSurfaceOperation {
            $($variant,)+
        }

        impl ApplicationSurfaceOperation {
            pub const ALL: [Self; count_operations!($($variant),+)] = [
                $(Self::$variant,)+
            ];

            pub const MCP_TOOL_NAMES: [&'static str; count_operations!($($variant),+)] = [
                $(
                    application_surface_operations!(
                        @mcp_tool_name $catalog_name $(, $mcp_name)?
                    ),
                )+
            ];

            pub fn from_catalog_name(name: &str) -> Option<Self> {
                Self::ALL
                    .into_iter()
                    .find(|operation| operation.as_str() == name)
            }

            pub fn from_tool_name(tool_name: &str) -> Option<Self> {
                let operation = tool_name
                    .strip_prefix("tracedecay_")
                    .unwrap_or(tool_name);
                Self::ALL
                    .into_iter()
                    .find(|candidate| candidate.mcp_operation_name() == operation)
            }

            pub fn from_surface_name(surface: crate::BindingSurface, name: &str) -> Option<Self> {
                Self::ALL
                    .into_iter()
                    .find(|candidate| candidate.name_for_surface(surface) == name)
            }

            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $catalog_name,)+
                }
            }

            /// MCP/CLI spelling for this canonical operation.
            ///
            /// The diagnostics read keeps its established public tool spelling;
            /// its catalog and HTTP/SDK identity remain `diagnostics_read`.
            pub const fn mcp_operation_name(self) -> &'static str {
                match self {
                    $(
                        Self::$variant => application_surface_operations!(
                            @mcp_name $catalog_name $(, $mcp_name)?
                        ),
                    )+
                }
            }

            pub const fn mcp_tool_name(self) -> &'static str {
                match self {
                    $(
                        Self::$variant => application_surface_operations!(
                            @mcp_tool_name $catalog_name $(, $mcp_name)?
                        ),
                    )+
                }
            }

            pub const fn name_for_surface(self, surface: crate::BindingSurface) -> &'static str {
                match surface {
                    crate::BindingSurface::Cli | crate::BindingSurface::Mcp => {
                        self.mcp_operation_name()
                    }
                    crate::BindingSurface::Http
                    | crate::BindingSurface::Lsp
                    | crate::BindingSurface::Dashboard => self.as_str(),
                }
            }
        }
    };
    (@mcp_name $catalog_name:literal, $mcp_name:literal) => {
        $mcp_name
    };
    (@mcp_name $catalog_name:literal) => {
        $catalog_name
    };
    (@mcp_tool_name $catalog_name:literal, $mcp_name:literal) => {
        concat!("tracedecay_", $mcp_name)
    };
    (@mcp_tool_name $catalog_name:literal) => {
        concat!("tracedecay_", $catalog_name)
    };
}

application_surface_operations! {
    GitStatus => "git_status";
    GitDiff => "git_diff";
    GitHistory => "git_history";
    GitBlame => "git_blame";
    GitHunks => "git_hunks";
    GitPreview => "git_preview";
    GitApply => "git_apply";
    GitHubStackSignalExpand => "github_stack_signal_expand";
    NativeIntegrationStackSnapshot => "stack_snapshot";
    NativeIntegrationPreflight => "preflight_native_integration";
    NativeIntegrationApprove => "approve_native_integration";
    NativeIntegrationApply => "apply_native_integration";
    NativeIntegrationStatus => "native_integration_status";
    NativeIntegrationCancel => "cancel_native_integration";
    NativeIntegrationWorktreeInventory => "worktree_inventory";
    NativeIntegrationWorktreeInspect => "worktree_cleanup_inspect";
    NativeIntegrationWorktreeConfirm => "worktree_cleanup_confirm";
    NativeIntegrationWorktreeRemove => "worktree_cleanup_remove";
    NativeIntegrationWorktreeReconcile => "worktree_cleanup_reconcile";
    FeedbackDiagnostics => "feedback_diagnostics";
    FeedbackGet => "feedback_get";
    FeedbackExpand => "feedback_expand";
    FeedbackList => "feedback_list";
    FeedbackImpact => "feedback_impact";
    FeedbackAdvisoryCycle => "feedback_advisory_cycle";
    AffectedTests => "affected_tests";
    TestResults => "test_results";
    CodeExactOccurrence => "code_exact_occurrence";
    CodePhraseSearch => "code_phrase_search";
    CodeSymbolSearch => "code_symbol_search";
    CodeSignatureSearch => "code_signature_search";
    CodeImplementations => "code_implementations";
    CodeTypeHierarchy => "code_type_hierarchy";
    CodeCallers => "code_callers";
    CodeCallees => "code_callees";
    CodeFacets => "code_facets";
    CodeTimeline => "code_timeline";
    CodeDeclaration => "code_declaration";
    CodeTypeDefinition => "code_type_definition";
    CodeReferences => "code_references";
    SessionLookup => "session_lookup";
    QualifiedName => "qualified_name";
    CallChain => "call_chain";
    FileDependents => "file_dependents";
    SourceLines => "source_lines";
    SourceBody => "source_body";
    SourceOutline => "source_outline";
    ModuleApi => "module_api";
    HealthRead => "health_read";
    HealthDelta => "health_delta";
    StorageStatus => "storage_status";
    DiagnosticsRead => "diagnostics_read", mcp: "diagnostics";
    ObservatoryRead => "observatory_read";
    ConfigurationList => "configuration_list";
    ConfigurationGet => "configuration_get";
    ConfigurationSet => "configuration_set";
    ConfigurationUnset => "configuration_unset";
    ConfigurationBatch => "configuration_batch";
    ConfigurationObservedState => "configuration_observed_state";
    ConfigurationProtectedPreview => "configuration_protected_preview";
    ConfigurationProtectedApply => "configuration_protected_apply";
    ConfigurationRollbackPreview => "configuration_rollback_preview";
    ConfigurationRollbackApply => "configuration_rollback_apply";
    ConfigurationAudit => "configuration_audit";
    ContextScoutStatus => "context_scout_status";
    ContextScoutRecent => "context_scout_recent";
    ContextScoutExplain => "context_scout_explain";
    ContextScoutCapability => "context_scout_capability";
    ContextScoutBudget => "context_scout_budget";
    ContextScoutPause => "context_scout_pause";
    ContextScoutResume => "context_scout_resume";
    ContextScoutCancel => "context_scout_cancel";
    ContextScoutClaim => "context_scout_claim";
    ContextScoutDelivery => "context_scout_delivery";
    ContextScoutFeedback => "context_scout_feedback";
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::ApplicationSurfaceOperation;

    #[test]
    fn canonical_names_are_unique_and_round_trip() {
        let names = ApplicationSurfaceOperation::ALL
            .into_iter()
            .map(ApplicationSurfaceOperation::as_str)
            .collect::<BTreeSet<_>>();
        assert_eq!(names.len(), ApplicationSurfaceOperation::ALL.len());
        for operation in ApplicationSurfaceOperation::ALL {
            assert_eq!(
                ApplicationSurfaceOperation::from_catalog_name(operation.as_str()),
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
    fn tool_names_resolve_the_transport_spelling_only() {
        for operation in ApplicationSurfaceOperation::ALL {
            assert_eq!(
                ApplicationSurfaceOperation::from_tool_name(operation.mcp_operation_name()),
                Some(operation)
            );
            assert_eq!(
                ApplicationSurfaceOperation::from_tool_name(operation.mcp_tool_name()),
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
            ApplicationSurfaceOperation::from_tool_name("tracedecay_diagnostics_read"),
            None
        );
        assert_eq!(
            ApplicationSurfaceOperation::from_tool_name("tracedecay_not_an_operation"),
            None
        );
    }
}
