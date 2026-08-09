//! Owner-family classification for canonical HTTP application operations.

use super::{HttpApplicationOperation, HttpApplicationOwnerKind};

impl HttpApplicationOperation {
    pub const fn owner_kind(self) -> HttpApplicationOwnerKind {
        match self {
            Self::GitStatus
            | Self::GitDiff
            | Self::GitHistory
            | Self::GitBlame
            | Self::GitHunks
            | Self::GitPreview
            | Self::GitApply
            | Self::GitHubStackSignalExpand => HttpApplicationOwnerKind::Git,
            Self::NativeIntegrationStackSnapshot
            | Self::NativeIntegrationPreflight
            | Self::NativeIntegrationApprove
            | Self::NativeIntegrationApply
            | Self::NativeIntegrationStatus
            | Self::NativeIntegrationCancel
            | Self::NativeIntegrationWorktreeInventory
            | Self::NativeIntegrationWorktreeInspect
            | Self::NativeIntegrationWorktreeConfirm
            | Self::NativeIntegrationWorktreeRemove
            | Self::NativeIntegrationWorktreeReconcile => {
                HttpApplicationOwnerKind::NativeIntegration
            }
            Self::FeedbackDiagnostics
            | Self::FeedbackGet
            | Self::FeedbackExpand
            | Self::FeedbackList
            | Self::FeedbackImpact
            | Self::FeedbackAdvisoryCycle
            | Self::AffectedTests => HttpApplicationOwnerKind::Feedback,
            Self::CodeExactOccurrence
            | Self::CodePhraseSearch
            | Self::CodeCallees
            | Self::CodeFacets
            | Self::CodeTimeline
            | Self::CodeDeclaration
            | Self::CodeDefinition
            | Self::CodeTypeDefinition
            | Self::CodeReferences => HttpApplicationOwnerKind::CallableCode,
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
            | Self::DiagnosticsRead => HttpApplicationOwnerKind::Primitive,
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
            | Self::ConfigurationAudit => HttpApplicationOwnerKind::Configuration,
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
            | Self::ContextScoutFeedback => HttpApplicationOwnerKind::ContextScout,
        }
    }
}
