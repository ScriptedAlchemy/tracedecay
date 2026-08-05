//! Canonical identities for application operations exposed through adapters.
//!
//! Transport crates own their route and framing projections. This module owns
//! the one operation vocabulary those projections and the root dispatcher
//! share.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_tool_catalog::{
    BindingId, BindingSurface, CapabilityId, CapabilityManifestV1, CatalogValidationError,
    SchemaBodyAuthorityV1, SurfaceBindingV1,
};

/// Canonical application owner family responsible for an operation.
#[derive(Clone, Copy, Debug, JsonSchema, PartialEq, Eq, Hash)]
pub enum ApplicationOwnerKind {
    Git,
    Feedback,
    CallableCode,
    Primitive,
    Configuration,
    ContextScout,
}

/// Canonical identity shared by every retained application surface.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationWireOperation {
    GitStatus,
    GitDiff,
    GitHistory,
    GitBlame,
    GitHunks,
    GitPreview,
    GitApply,
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
    ConfigurationReset,
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

/// Concrete request and result schema bodies for one operation.
///
/// Executability is deliberately absent. The composition root joins schemas
/// with independently verified service and route availability.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ApplicationWireSchemaV1 {
    operation: ApplicationWireOperation,
    capability_id: CapabilityId,
    binding_id: BindingId,
    surface: BindingSurface,
    request: SchemaBodyAuthorityV1,
    result: SchemaBodyAuthorityV1,
}

impl ApplicationWireSchemaV1 {
    pub fn from_catalog(
        operation: ApplicationWireOperation,
        manifest: &CapabilityManifestV1,
        binding: &SurfaceBindingV1,
        request: SchemaBodyAuthorityV1,
        result: SchemaBodyAuthorityV1,
    ) -> Result<Self, CatalogValidationError> {
        if binding.capability_id() != manifest.capability_id()
            || manifest
                .binding_ids()
                .binary_search(binding.binding_id())
                .is_err()
            || ApplicationWireOperation::from_catalog_name(binding.operation().as_str())
                != Some(operation)
            || request.schema_ref() != manifest.request_schema()
            || result.schema_ref() != manifest.result_schema()
        {
            return Err(CatalogValidationError::InvalidCapability {
                capability_id: manifest.capability_id().clone(),
                reason: "wire schema authority does not match its catalog binding",
            });
        }
        Ok(Self {
            operation,
            capability_id: manifest.capability_id().clone(),
            binding_id: binding.binding_id().clone(),
            surface: binding.surface(),
            request,
            result,
        })
    }

    pub const fn operation(&self) -> ApplicationWireOperation {
        self.operation
    }

    pub fn capability_id(&self) -> &CapabilityId {
        &self.capability_id
    }

    pub fn binding_id(&self) -> &BindingId {
        &self.binding_id
    }

    pub const fn surface(&self) -> BindingSurface {
        self.surface
    }

    pub fn request(&self) -> &SchemaBodyAuthorityV1 {
        &self.request
    }

    pub fn result(&self) -> &SchemaBodyAuthorityV1 {
        &self.result
    }
}

/// Canonically ordered schema authority assembled from DTO-owning crates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplicationWireSchemaRegistryV1 {
    schemas: BTreeMap<BindingId, ApplicationWireSchemaV1>,
}

impl ApplicationWireSchemaRegistryV1 {
    pub fn new(schemas: Vec<ApplicationWireSchemaV1>) -> Result<Self, CatalogValidationError> {
        let mut registry = BTreeMap::new();
        for schema in schemas {
            if registry
                .insert(schema.binding_id().clone(), schema)
                .is_some()
            {
                return Err(CatalogValidationError::DuplicateValue {
                    field: "application wire schema bindings",
                });
            }
        }
        Ok(Self { schemas: registry })
    }

    pub fn get(&self, binding_id: &BindingId) -> Option<&ApplicationWireSchemaV1> {
        self.schemas.get(binding_id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &ApplicationWireSchemaV1> {
        self.schemas.values()
    }
}

impl ApplicationWireOperation {
    pub const ALL: [Self; 67] = [
        Self::GitStatus,
        Self::GitDiff,
        Self::GitHistory,
        Self::GitBlame,
        Self::GitHunks,
        Self::GitPreview,
        Self::GitApply,
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
        Self::ConfigurationReset,
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
        let operation = tool_name.strip_prefix("tracedecay_").unwrap_or(tool_name);
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
            Self::ConfigurationReset => "configuration_reset",
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

    pub const fn owner_kind(self) -> ApplicationOwnerKind {
        match self {
            Self::GitStatus
            | Self::GitDiff
            | Self::GitHistory
            | Self::GitBlame
            | Self::GitHunks
            | Self::GitPreview
            | Self::GitApply => ApplicationOwnerKind::Git,
            Self::FeedbackDiagnostics
            | Self::FeedbackGet
            | Self::FeedbackExpand
            | Self::FeedbackList
            | Self::FeedbackImpact
            | Self::FeedbackAdvisoryCycle
            | Self::AffectedTests => ApplicationOwnerKind::Feedback,
            Self::CodeExactOccurrence
            | Self::CodePhraseSearch
            | Self::CodeCallees
            | Self::CodeFacets
            | Self::CodeTimeline
            | Self::CodeDeclaration
            | Self::CodeDefinition
            | Self::CodeTypeDefinition
            | Self::CodeReferences => ApplicationOwnerKind::CallableCode,
            Self::TestResults
            | Self::CodeSymbolSearch
            | Self::CodeSignatureSearch
            | Self::CodeImplementations
            | Self::CodeTypeHierarchy
            | Self::CodeCallers
            | Self::SessionLookup
            | Self::QualifiedName
            | Self::CallChain
            | Self::FileDependents
            | Self::SourceLines
            | Self::SourceBody
            | Self::SourceOutline
            | Self::ModuleApi
            | Self::FileMetadata
            | Self::HealthRead
            | Self::HealthDelta
            | Self::StorageStatus
            | Self::DiagnosticsRead => ApplicationOwnerKind::Primitive,
            Self::ConfigurationList
            | Self::ConfigurationExplain
            | Self::ConfigurationGet
            | Self::ConfigurationSet
            | Self::ConfigurationUnset
            | Self::ConfigurationBatch
            | Self::ConfigurationWriteCredential
            | Self::ConfigurationObservedState
            | Self::ConfigurationProtectedPreview
            | Self::ConfigurationProtectedApply
            | Self::ConfigurationRollbackPreview
            | Self::ConfigurationRollbackApply
            | Self::ConfigurationAudit
            | Self::ConfigurationReset => ApplicationOwnerKind::Configuration,
            Self::ContextScoutStatus
            | Self::ContextScoutRecent
            | Self::ContextScoutExplain
            | Self::ContextScoutCapability
            | Self::ContextScoutBudget
            | Self::ContextScoutPause
            | Self::ContextScoutResume
            | Self::ContextScoutCancel
            | Self::ContextScoutClaim
            | Self::ContextScoutDelivery
            | Self::ContextScoutFeedback => ApplicationOwnerKind::ContextScout,
        }
    }

    pub const fn is_callable_code(self) -> bool {
        matches!(
            self,
            Self::CodeExactOccurrence
                | Self::CodePhraseSearch
                | Self::CodeSymbolSearch
                | Self::CodeSignatureSearch
                | Self::CodeImplementations
                | Self::CodeTypeHierarchy
                | Self::CodeCallers
                | Self::CodeCallees
                | Self::CodeFacets
                | Self::CodeTimeline
                | Self::CodeDeclaration
                | Self::CodeDefinition
                | Self::CodeTypeDefinition
                | Self::CodeReferences
        )
    }
}

#[cfg(test)]
mod tests {
    use super::ApplicationWireOperation;

    #[test]
    fn operation_names_round_trip_without_duplicates() {
        let mut names = ApplicationWireOperation::ALL
            .into_iter()
            .map(ApplicationWireOperation::as_str)
            .collect::<Vec<_>>();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert!(!names.is_empty());
        assert_eq!(names.len(), count);
        for operation in ApplicationWireOperation::ALL {
            assert_eq!(
                ApplicationWireOperation::from_catalog_name(operation.as_str()),
                Some(operation)
            );
        }
    }
}
