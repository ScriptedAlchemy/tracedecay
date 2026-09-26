//! API-owned owner-family classification for canonical HTTP operations.

use tracedecay_tool_catalog::ApplicationSurfaceOperation;

use super::HttpApplicationOwnerKind;

/// `None` for operations that have no HTTP binding.
pub const fn http_application_owner_kind(
    operation: ApplicationSurfaceOperation,
) -> Option<HttpApplicationOwnerKind> {
    Some(match operation {
        ApplicationSurfaceOperation::GitStatus
        | ApplicationSurfaceOperation::GitDiff
        | ApplicationSurfaceOperation::GitHistory
        | ApplicationSurfaceOperation::GitBlame
        | ApplicationSurfaceOperation::GitHunks
        | ApplicationSurfaceOperation::GitPreview
        | ApplicationSurfaceOperation::GitApply
        | ApplicationSurfaceOperation::GitHubStackSignalExpand => HttpApplicationOwnerKind::Git,
        ApplicationSurfaceOperation::NativeIntegrationStackSnapshot
        | ApplicationSurfaceOperation::NativeIntegrationPreflight
        | ApplicationSurfaceOperation::NativeIntegrationApprove
        | ApplicationSurfaceOperation::NativeIntegrationApply
        | ApplicationSurfaceOperation::NativeIntegrationStatus
        | ApplicationSurfaceOperation::NativeIntegrationCancel
        | ApplicationSurfaceOperation::NativeIntegrationWorktreeInventory
        | ApplicationSurfaceOperation::NativeIntegrationWorktreeInspect
        | ApplicationSurfaceOperation::NativeIntegrationWorktreeConfirm
        | ApplicationSurfaceOperation::NativeIntegrationWorktreeRemove
        | ApplicationSurfaceOperation::NativeIntegrationWorktreeReconcile => {
            HttpApplicationOwnerKind::NativeIntegration
        }
        ApplicationSurfaceOperation::FeedbackDiagnostics
        | ApplicationSurfaceOperation::FeedbackGet
        | ApplicationSurfaceOperation::FeedbackExpand
        | ApplicationSurfaceOperation::FeedbackList
        | ApplicationSurfaceOperation::FeedbackImpact
        | ApplicationSurfaceOperation::FeedbackAdvisoryCycle
        | ApplicationSurfaceOperation::FeedbackProximity
        | ApplicationSurfaceOperation::AffectedTests => HttpApplicationOwnerKind::Feedback,
        ApplicationSurfaceOperation::CodeExactOccurrence
        | ApplicationSurfaceOperation::CodePhraseSearch
        | ApplicationSurfaceOperation::CodeCallees
        | ApplicationSurfaceOperation::CodeFacets
        | ApplicationSurfaceOperation::CodeTimeline
        | ApplicationSurfaceOperation::CodeDeclaration
        | ApplicationSurfaceOperation::CodeTypeDefinition
        | ApplicationSurfaceOperation::CodeReferences => HttpApplicationOwnerKind::CallableCode,
        ApplicationSurfaceOperation::TestResults
        | ApplicationSurfaceOperation::CodeSymbolSearch
        | ApplicationSurfaceOperation::CodeSignatureSearch
        | ApplicationSurfaceOperation::CodeImplementations
        | ApplicationSurfaceOperation::CodeTypeHierarchy
        | ApplicationSurfaceOperation::CodeCallers
        | ApplicationSurfaceOperation::SessionLookup
        | ApplicationSurfaceOperation::QualifiedName
        | ApplicationSurfaceOperation::CallChain
        | ApplicationSurfaceOperation::FileDependents
        | ApplicationSurfaceOperation::SourceLines
        | ApplicationSurfaceOperation::SourceBody
        | ApplicationSurfaceOperation::SourceOutline
        | ApplicationSurfaceOperation::ModuleApi
        | ApplicationSurfaceOperation::HealthRead
        | ApplicationSurfaceOperation::HealthDelta
        | ApplicationSurfaceOperation::StorageStatus
        | ApplicationSurfaceOperation::DiagnosticsRead => HttpApplicationOwnerKind::Primitive,
        ApplicationSurfaceOperation::ObservatoryRead => HttpApplicationOwnerKind::Observatory,
        ApplicationSurfaceOperation::ConfigurationList
        | ApplicationSurfaceOperation::ConfigurationGet
        | ApplicationSurfaceOperation::ConfigurationSet
        | ApplicationSurfaceOperation::ConfigurationUnset
        | ApplicationSurfaceOperation::ConfigurationBatch
        | ApplicationSurfaceOperation::ConfigurationObservedState
        | ApplicationSurfaceOperation::ConfigurationProtectedPreview
        | ApplicationSurfaceOperation::ConfigurationProtectedApply
        | ApplicationSurfaceOperation::ConfigurationRollbackPreview
        | ApplicationSurfaceOperation::ConfigurationRollbackApply
        | ApplicationSurfaceOperation::ConfigurationAudit => {
            HttpApplicationOwnerKind::Configuration
        }
        ApplicationSurfaceOperation::ContextScoutStatus
        | ApplicationSurfaceOperation::ContextScoutRecent
        | ApplicationSurfaceOperation::ContextScoutExplain
        | ApplicationSurfaceOperation::ContextScoutCapability
        | ApplicationSurfaceOperation::ContextScoutBudget
        | ApplicationSurfaceOperation::ContextScoutPause
        | ApplicationSurfaceOperation::ContextScoutResume
        | ApplicationSurfaceOperation::ContextScoutCancel
        | ApplicationSurfaceOperation::ContextScoutClaim
        | ApplicationSurfaceOperation::ContextScoutDelivery
        | ApplicationSurfaceOperation::ContextScoutFeedback => {
            HttpApplicationOwnerKind::ContextScout
        }
        ApplicationSurfaceOperation::StrReplace
        | ApplicationSurfaceOperation::MultiStrReplace
        | ApplicationSurfaceOperation::InsertAt
        | ApplicationSurfaceOperation::AstGrepRewrite
        | ApplicationSurfaceOperation::ReplaceSymbol
        | ApplicationSurfaceOperation::InsertAtSymbol
        | ApplicationSurfaceOperation::MoveSymbol
        | ApplicationSurfaceOperation::RenameSymbol
        | ApplicationSurfaceOperation::SourceEditReconcile
        | ApplicationSurfaceOperation::SourceEditRollback
        | ApplicationSurfaceOperation::Context
        | ApplicationSurfaceOperation::Node
        | ApplicationSurfaceOperation::Impact
        | ApplicationSurfaceOperation::Similar
        | ApplicationSurfaceOperation::Redundancy
        | ApplicationSurfaceOperation::RenamePreview
        | ApplicationSurfaceOperation::PortStatus
        | ApplicationSurfaceOperation::PortOrder
        | ApplicationSurfaceOperation::Todos
        | ApplicationSurfaceOperation::TestMap
        | ApplicationSurfaceOperation::TestRisk
        | ApplicationSurfaceOperation::Gini
        | ApplicationSurfaceOperation::DependencyDepth
        | ApplicationSurfaceOperation::Health
        | ApplicationSurfaceOperation::Dsm
        | ApplicationSurfaceOperation::Diagnose
        | ApplicationSurfaceOperation::DeadCode
        | ApplicationSurfaceOperation::Circular
        | ApplicationSurfaceOperation::Hotspots
        | ApplicationSurfaceOperation::UnmountedFiles
        | ApplicationSurfaceOperation::Rank
        | ApplicationSurfaceOperation::Largest
        | ApplicationSurfaceOperation::Coupling
        | ApplicationSurfaceOperation::InheritanceDepth
        | ApplicationSurfaceOperation::Distribution
        | ApplicationSurfaceOperation::Recursion
        | ApplicationSurfaceOperation::Complexity
        | ApplicationSurfaceOperation::DocCoverage
        | ApplicationSurfaceOperation::GodClass
        | ApplicationSurfaceOperation::UnsafePatterns
        | ApplicationSurfaceOperation::Constructors
        | ApplicationSurfaceOperation::FieldSites
        | ApplicationSurfaceOperation::FindExactSymbol
        | ApplicationSurfaceOperation::ByQualifiedName
        | ApplicationSurfaceOperation::Signature
        | ApplicationSurfaceOperation::Derives
        | ApplicationSurfaceOperation::Grep
        | ApplicationSurfaceOperation::AstGrepSearch => return None,
        ApplicationSurfaceOperation::FactStoreCurate
        | ApplicationSurfaceOperation::FactStoreAdd
        | ApplicationSurfaceOperation::FactStoreSearch
        | ApplicationSurfaceOperation::FactStoreProbe
        | ApplicationSurfaceOperation::FactStoreRelated
        | ApplicationSurfaceOperation::FactStoreReason
        | ApplicationSurfaceOperation::FactStoreContradict
        | ApplicationSurfaceOperation::FactStoreGet
        | ApplicationSurfaceOperation::FactStoreUpdate
        | ApplicationSurfaceOperation::FactStoreRemove
        | ApplicationSurfaceOperation::FactStoreSupersede
        | ApplicationSurfaceOperation::FactStoreList
        | ApplicationSurfaceOperation::FactFeedback
        | ApplicationSurfaceOperation::MemoryStatus
        | ApplicationSurfaceOperation::SessionRefreshStatus
        | ApplicationSurfaceOperation::SessionRefreshCancel
        | ApplicationSurfaceOperation::SessionRefreshBegin
        | ApplicationSurfaceOperation::MessageSearch
        | ApplicationSurfaceOperation::SessionsFor
        | ApplicationSurfaceOperation::Workflows
        | ApplicationSurfaceOperation::LcmStatus
        | ApplicationSurfaceOperation::LcmDoctor
        | ApplicationSurfaceOperation::LcmLoadSession
        | ApplicationSurfaceOperation::LcmGrep
        | ApplicationSurfaceOperation::LcmDescribe
        | ApplicationSurfaceOperation::LcmExpand
        | ApplicationSurfaceOperation::LcmExpandQuery => HttpApplicationOwnerKind::Retained,
    })
}
