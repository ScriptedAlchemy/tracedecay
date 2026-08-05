use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tracedecay_application::context_scout::{
    ContextScoutBudgetStateV1, ContextScoutCancelRequestV1, ContextScoutCapabilityStateV1,
    ContextScoutClaimRequestV1, ContextScoutClaimResultV1, ContextScoutControlRequestV1,
    ContextScoutDeliveryRequestV1, ContextScoutExactAddressRequestV1, ContextScoutExplanationV1,
    ContextScoutFeedbackRequestV1, ContextScoutMutationResultV1, ContextScoutRecentRequestV1,
    ContextScoutRecentStateV1, ContextScoutStatusV1,
};
use tracedecay_application::feedback::{
    FeedbackAdvisoryCycleRequestV1, FeedbackDiagnosticsReadResultV1, FeedbackExpandResultV1,
    FeedbackGetResultV1, FeedbackHandleRequestV1, FeedbackListResultV1,
};
use tracedecay_application::retrieval::{
    CodeCalleesSurfaceRequest, CodeCallersSurfaceRequest, CodeExactOccurrenceSurfaceRequest,
    CodeFacetRecord, CodeFacetSurfaceRequest, CodeImplementationsSurfaceRequest,
    CodeNavigationSurfaceRequest, CodePhraseSearchSurfaceRequest, CodeQueryPage,
    CodeSignatureSearchSurfaceRequest, CodeSymbolSearchSurfaceRequest, CodeTimelineRecord,
    CodeTimelineSurfaceRequest, CodeTypeHierarchySurfaceRequest, ExactOccurrenceRecord,
    HealthDeltaRequest, HealthDeltaResult, HealthReadRequest, HealthReadResult,
    LexicalOccurrenceRecord, SessionLookupRequest, SessionLookupResult, SourceLinesRequest,
    SourceLinesResult, SymbolGraphPage, SymbolPrimitiveRecord, SymbolRelationRecord,
    TestResultsRequestV1, TestResultsResultV1, TypeHierarchyRecord,
};
use tracedecay_application::{
    ApplicationOutcome, ApplicationWireOperation, ApplicationWireSchemaRegistryV1,
    ApplicationWireSchemaV1, ConfigurationAuditRequestV1, ConfigurationBatchRequestV1,
    ConfigurationGetRequestV1, ConfigurationListRequestV1, ConfigurationObservedStateRequestV1,
    ConfigurationProtectedApplyRequestV1, ConfigurationProtectedPreviewRequestV1,
    ConfigurationResetOutcomeV1, ConfigurationResetRequestV1, ConfigurationRollbackApplyRequestV1,
    ConfigurationRollbackPreviewRequestV1, ConfigurationSetRequestV1, ConfigurationUnsetRequestV1,
    ConfigurationWriteCredentialRequestV1, EffectResult, PreviewResult,
};
use tracedecay_domain::configuration::{CredentialReferenceMetadataV1, ProtectedChangePlan};
use tracedecay_domain::{GitIndexPreviewV1, GitIndexTransactionReceiptV1};
use tracedecay_tool_catalog::{CatalogSnapshotV1, SchemaBodyAuthorityV1};
use tracedecay_usecases::configuration::{
    ComponentConfigurationState, ConfigurationAuditPage, ConfigurationMutationReceipt,
    ResolvedSetting, SettingSummary,
};
use tracedecay_usecases::feedback::FeedbackAdvisoryCycleResultV1;
use tracedecay_usecases::feedback::owner::{
    CanonicalAffectedTestsProjectionV1, CanonicalFeedbackImpactProjectionV1,
};
use tracedecay_usecases::git_reads::{
    GitApplySurfaceRequest, GitPreviewSurfaceRequest, GitReadResultV1, GitReadSurfaceRequest,
};
use tracedecay_usecases::primitives::{
    CallChainPrimitiveRequest, CallChainPrimitiveResult, DiagnosticsPrimitiveRequest,
    DiagnosticsPrimitiveResult, FileDependentsPrimitiveRequest, FileDependentsPrimitiveResult,
    FileMetadataPrimitiveRequest, FileMetadataPrimitiveResult, ModuleApiPrimitiveRequest,
    ModuleApiPrimitiveResult, QualifiedNamePrimitiveRequest, QualifiedNamePrimitiveResult,
    SourceBodyPrimitiveRequest, SourceBodyPrimitiveResult, SourceOutlinePrimitiveRequest,
    SourceOutlinePrimitiveResult, StorageStatusPrimitiveRequest, StorageStatusPrimitiveResult,
};

use super::{ApplicationSurfaceAdapterError, ApplicationSurfaceOperation};

fn add_schema<Request, Result>(
    catalog: &CatalogSnapshotV1,
    operation: ApplicationWireOperation,
    schemas: &mut Vec<ApplicationWireSchemaV1>,
) -> Result<(), ApplicationSurfaceAdapterError>
where
    Request: JsonSchema,
    Result: JsonSchema,
{
    let manifest = catalog
        .capabilities()
        .find(|manifest| {
            manifest.binding_ids().iter().any(|binding_id| {
                catalog
                    .binding(binding_id)
                    .is_some_and(|binding| binding.operation().as_str() == operation.as_str())
            })
        })
        .ok_or(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized)?;
    let request = SchemaBodyAuthorityV1::for_type::<Request>(manifest.request_schema().clone())?;
    let result = SchemaBodyAuthorityV1::for_type::<Result>(manifest.result_schema().clone())?;
    for binding_id in manifest.binding_ids() {
        let binding = catalog
            .binding(binding_id)
            .ok_or(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized)?;
        schemas.push(ApplicationWireSchemaV1::from_catalog(
            operation,
            manifest,
            binding,
            request.clone(),
            result.clone(),
        )?);
    }
    Ok(())
}

pub(super) fn build_application_wire_schema_registry(
    catalog: &CatalogSnapshotV1,
) -> Result<ApplicationWireSchemaRegistryV1, ApplicationSurfaceAdapterError> {
    let mut schemas = Vec::new();
    macro_rules! add {
        ($operation:ident, $request:ty, $result:ty) => {
            add_schema::<$request, $result>(
                catalog,
                ApplicationWireOperation::$operation,
                &mut schemas,
            )?
        };
    }
    add!(GitStatus, GitReadSurfaceRequest, GitReadResultV1);
    add!(GitDiff, GitReadSurfaceRequest, GitReadResultV1);
    add!(GitHistory, GitReadSurfaceRequest, GitReadResultV1);
    add!(GitBlame, GitReadSurfaceRequest, GitReadResultV1);
    add!(GitHunks, GitReadSurfaceRequest, GitReadResultV1);
    add!(
        GitPreview,
        GitPreviewSurfaceRequest,
        PreviewResult<GitIndexPreviewV1>
    );
    add!(
        GitApply,
        GitApplySurfaceRequest,
        EffectResult<GitIndexTransactionReceiptV1>
    );
    add!(
        FeedbackDiagnostics,
        FeedbackHandleRequestV1,
        FeedbackDiagnosticsReadResultV1
    );
    add!(FeedbackGet, FeedbackHandleRequestV1, FeedbackGetResultV1);
    add!(
        FeedbackExpand,
        FeedbackHandleRequestV1,
        FeedbackExpandResultV1
    );
    add!(FeedbackList, FeedbackHandleRequestV1, FeedbackListResultV1);
    add!(
        FeedbackImpact,
        FeedbackHandleRequestV1,
        CanonicalFeedbackImpactProjectionV1
    );
    add!(
        FeedbackAdvisoryCycle,
        FeedbackAdvisoryCycleRequestV1,
        FeedbackAdvisoryCycleResultV1
    );
    add!(
        AffectedTests,
        FeedbackHandleRequestV1,
        CanonicalAffectedTestsProjectionV1
    );
    add!(TestResults, TestResultsRequestV1, TestResultsResultV1);
    add!(
        CodeExactOccurrence,
        CodeExactOccurrenceSurfaceRequest,
        CodeQueryPage<ExactOccurrenceRecord>
    );
    add!(
        CodePhraseSearch,
        CodePhraseSearchSurfaceRequest,
        CodeQueryPage<LexicalOccurrenceRecord>
    );
    add!(
        CodeSymbolSearch,
        CodeSymbolSearchSurfaceRequest,
        SymbolGraphPage<SymbolPrimitiveRecord>
    );
    add!(
        CodeSignatureSearch,
        CodeSignatureSearchSurfaceRequest,
        SymbolGraphPage<SymbolPrimitiveRecord>
    );
    add!(
        CodeImplementations,
        CodeImplementationsSurfaceRequest,
        SymbolGraphPage<SymbolRelationRecord>
    );
    add!(
        CodeTypeHierarchy,
        CodeTypeHierarchySurfaceRequest,
        SymbolGraphPage<TypeHierarchyRecord>
    );
    add!(
        CodeCallers,
        CodeCallersSurfaceRequest,
        SymbolGraphPage<SymbolRelationRecord>
    );
    add!(
        CodeCallees,
        CodeCalleesSurfaceRequest,
        CodeQueryPage<SymbolRelationRecord>
    );
    add!(
        CodeFacets,
        CodeFacetSurfaceRequest,
        CodeQueryPage<CodeFacetRecord>
    );
    add!(
        CodeTimeline,
        CodeTimelineSurfaceRequest,
        CodeQueryPage<CodeTimelineRecord>
    );
    add!(
        CodeDeclaration,
        CodeNavigationSurfaceRequest,
        CodeQueryPage<SymbolPrimitiveRecord>
    );
    add!(
        CodeDefinition,
        CodeNavigationSurfaceRequest,
        CodeQueryPage<SymbolPrimitiveRecord>
    );
    add!(
        CodeTypeDefinition,
        CodeNavigationSurfaceRequest,
        CodeQueryPage<SymbolPrimitiveRecord>
    );
    add!(
        CodeReferences,
        CodeNavigationSurfaceRequest,
        CodeQueryPage<SymbolRelationRecord>
    );
    add!(SessionLookup, SessionLookupRequest, SessionLookupResult);
    add!(
        QualifiedName,
        QualifiedNamePrimitiveRequest,
        QualifiedNamePrimitiveResult
    );
    add!(
        CallChain,
        CallChainPrimitiveRequest,
        CallChainPrimitiveResult
    );
    add!(
        FileDependents,
        FileDependentsPrimitiveRequest,
        FileDependentsPrimitiveResult
    );
    add!(SourceLines, SourceLinesRequest, SourceLinesResult);
    add!(
        SourceBody,
        SourceBodyPrimitiveRequest,
        SourceBodyPrimitiveResult
    );
    add!(
        SourceOutline,
        SourceOutlinePrimitiveRequest,
        SourceOutlinePrimitiveResult
    );
    add!(
        ModuleApi,
        ModuleApiPrimitiveRequest,
        ModuleApiPrimitiveResult
    );
    add!(
        FileMetadata,
        FileMetadataPrimitiveRequest,
        FileMetadataPrimitiveResult
    );
    add!(HealthRead, HealthReadRequest, HealthReadResult);
    add!(HealthDelta, HealthDeltaRequest, HealthDeltaResult);
    add!(
        StorageStatus,
        StorageStatusPrimitiveRequest,
        StorageStatusPrimitiveResult
    );
    add!(
        DiagnosticsRead,
        DiagnosticsPrimitiveRequest,
        DiagnosticsPrimitiveResult
    );
    add!(
        ConfigurationList,
        ConfigurationListRequestV1,
        Vec<SettingSummary>
    );
    add!(
        ConfigurationExplain,
        ConfigurationGetRequestV1,
        ResolvedSetting
    );
    add!(ConfigurationGet, ConfigurationGetRequestV1, ResolvedSetting);
    add!(
        ConfigurationSet,
        ConfigurationSetRequestV1,
        ConfigurationMutationReceipt
    );
    add!(
        ConfigurationUnset,
        ConfigurationUnsetRequestV1,
        ConfigurationMutationReceipt
    );
    add!(
        ConfigurationBatch,
        ConfigurationBatchRequestV1,
        ConfigurationMutationReceipt
    );
    add!(
        ConfigurationWriteCredential,
        ConfigurationWriteCredentialRequestV1,
        CredentialReferenceMetadataV1
    );
    add!(
        ConfigurationObservedState,
        ConfigurationObservedStateRequestV1,
        Vec<ComponentConfigurationState>
    );
    add!(
        ConfigurationProtectedPreview,
        ConfigurationProtectedPreviewRequestV1,
        ProtectedChangePlan
    );
    add!(
        ConfigurationProtectedApply,
        ConfigurationProtectedApplyRequestV1,
        ConfigurationMutationReceipt
    );
    add!(
        ConfigurationRollbackPreview,
        ConfigurationRollbackPreviewRequestV1,
        ProtectedChangePlan
    );
    add!(
        ConfigurationRollbackApply,
        ConfigurationRollbackApplyRequestV1,
        ConfigurationMutationReceipt
    );
    add!(
        ConfigurationAudit,
        ConfigurationAuditRequestV1,
        ConfigurationAuditPage
    );
    add!(
        ConfigurationReset,
        ConfigurationResetRequestV1,
        ConfigurationResetOutcomeV1
    );
    add!(
        ContextScoutStatus,
        ContextScoutExactAddressRequestV1,
        ContextScoutStatusV1
    );
    add!(
        ContextScoutRecent,
        ContextScoutRecentRequestV1,
        ContextScoutRecentStateV1
    );
    add!(
        ContextScoutExplain,
        ContextScoutRecentRequestV1,
        ContextScoutExplanationV1
    );
    add!(
        ContextScoutCapability,
        ContextScoutExactAddressRequestV1,
        ContextScoutCapabilityStateV1
    );
    add!(
        ContextScoutBudget,
        ContextScoutExactAddressRequestV1,
        ContextScoutBudgetStateV1
    );
    add!(
        ContextScoutPause,
        ContextScoutControlRequestV1,
        ConfigurationMutationReceipt
    );
    add!(
        ContextScoutResume,
        ContextScoutControlRequestV1,
        ConfigurationMutationReceipt
    );
    add!(
        ContextScoutCancel,
        ContextScoutCancelRequestV1,
        ContextScoutMutationResultV1
    );
    add!(
        ContextScoutClaim,
        ContextScoutClaimRequestV1,
        ContextScoutClaimResultV1
    );
    add!(
        ContextScoutDelivery,
        ContextScoutDeliveryRequestV1,
        ContextScoutMutationResultV1
    );
    add!(
        ContextScoutFeedback,
        ContextScoutFeedbackRequestV1,
        ContextScoutMutationResultV1
    );
    ApplicationWireSchemaRegistryV1::new(schemas).map_err(Into::into)
}

fn payload_decodes<T: DeserializeOwned>(payload: Option<&Value>) -> bool {
    payload.is_none_or(|value| serde_json::from_value::<T>(value.clone()).is_ok())
}

fn evidence_decodes<T: DeserializeOwned>(outcome: &ApplicationOutcome<Value>) -> bool {
    matches!(
        outcome,
        ApplicationOutcome::Evidence(packet) if payload_decodes::<T>(packet.payload.as_ref())
    )
}

fn preview_decodes<T: DeserializeOwned>(outcome: &ApplicationOutcome<Value>) -> bool {
    matches!(
        outcome,
        ApplicationOutcome::Preview(preview) if payload_decodes::<T>(preview.payload.as_ref())
    )
}

fn effect_decodes<T: DeserializeOwned>(outcome: &ApplicationOutcome<Value>) -> bool {
    matches!(
        outcome,
        ApplicationOutcome::Effect(effect) if payload_decodes::<T>(effect.payload.as_ref())
    )
}

/// Validate the transport serialization carrier against the concrete result
/// DTO before an adapter can publish it.
pub(super) fn validate_configuration_outcome(
    operation: ApplicationSurfaceOperation,
    outcome: &ApplicationOutcome<Value>,
) -> bool {
    match (operation, outcome) {
        (ApplicationSurfaceOperation::ConfigurationList, ApplicationOutcome::Evidence(packet)) => {
            payload_decodes::<Vec<SettingSummary>>(packet.payload.as_ref())
        }
        (
            ApplicationSurfaceOperation::ConfigurationExplain
            | ApplicationSurfaceOperation::ConfigurationGet,
            ApplicationOutcome::Evidence(packet),
        ) => payload_decodes::<ResolvedSetting>(packet.payload.as_ref()),
        (
            ApplicationSurfaceOperation::ConfigurationObservedState,
            ApplicationOutcome::Evidence(packet),
        ) => payload_decodes::<Vec<ComponentConfigurationState>>(packet.payload.as_ref()),
        (ApplicationSurfaceOperation::ConfigurationAudit, ApplicationOutcome::Evidence(packet)) => {
            payload_decodes::<ConfigurationAuditPage>(packet.payload.as_ref())
        }
        (
            ApplicationSurfaceOperation::ConfigurationProtectedPreview
            | ApplicationSurfaceOperation::ConfigurationRollbackPreview,
            ApplicationOutcome::Preview(preview),
        ) => payload_decodes::<ProtectedChangePlan>(preview.payload.as_ref()),
        (
            ApplicationSurfaceOperation::ConfigurationWriteCredential,
            ApplicationOutcome::Effect(effect),
        ) => payload_decodes::<CredentialReferenceMetadataV1>(effect.payload.as_ref()),
        (
            ApplicationSurfaceOperation::ConfigurationSet
            | ApplicationSurfaceOperation::ConfigurationUnset
            | ApplicationSurfaceOperation::ConfigurationBatch
            | ApplicationSurfaceOperation::ConfigurationProtectedApply
            | ApplicationSurfaceOperation::ConfigurationRollbackApply,
            ApplicationOutcome::Effect(effect),
        ) => payload_decodes::<ConfigurationMutationReceipt>(effect.payload.as_ref()),
        (ApplicationSurfaceOperation::ConfigurationReset, ApplicationOutcome::Evidence(packet)) => {
            payload_decodes::<ConfigurationResetOutcomeV1>(packet.payload.as_ref())
        }
        _ => false,
    }
}

/// Fail closed when a daemon response's payload does not match the concrete
/// result DTO registered for the operation. An admitted unavailable outcome
/// may omit its payload, but it must still use the operation's outcome class.
pub(super) fn validate_application_outcome(
    operation: ApplicationSurfaceOperation,
    outcome: &ApplicationOutcome<Value>,
) -> bool {
    macro_rules! evidence {
        ($result:ty) => {
            evidence_decodes::<$result>(outcome)
        };
    }
    match operation {
        ApplicationSurfaceOperation::GitStatus
        | ApplicationSurfaceOperation::GitDiff
        | ApplicationSurfaceOperation::GitHistory
        | ApplicationSurfaceOperation::GitBlame
        | ApplicationSurfaceOperation::GitHunks => evidence!(GitReadResultV1),
        ApplicationSurfaceOperation::GitPreview => preview_decodes::<GitIndexPreviewV1>(outcome),
        ApplicationSurfaceOperation::GitApply => {
            effect_decodes::<GitIndexTransactionReceiptV1>(outcome)
        }
        ApplicationSurfaceOperation::FeedbackDiagnostics => {
            evidence!(FeedbackDiagnosticsReadResultV1)
        }
        ApplicationSurfaceOperation::FeedbackGet => evidence!(FeedbackGetResultV1),
        ApplicationSurfaceOperation::FeedbackExpand => evidence!(FeedbackExpandResultV1),
        ApplicationSurfaceOperation::FeedbackList => evidence!(FeedbackListResultV1),
        ApplicationSurfaceOperation::FeedbackImpact => {
            evidence!(CanonicalFeedbackImpactProjectionV1)
        }
        ApplicationSurfaceOperation::FeedbackAdvisoryCycle => {
            evidence!(FeedbackAdvisoryCycleResultV1)
        }
        ApplicationSurfaceOperation::AffectedTests => {
            evidence!(CanonicalAffectedTestsProjectionV1)
        }
        ApplicationSurfaceOperation::TestResults => evidence!(TestResultsResultV1),
        ApplicationSurfaceOperation::CodeExactOccurrence => {
            evidence!(CodeQueryPage<ExactOccurrenceRecord>)
        }
        ApplicationSurfaceOperation::CodePhraseSearch => {
            evidence!(CodeQueryPage<LexicalOccurrenceRecord>)
        }
        ApplicationSurfaceOperation::CodeSymbolSearch
        | ApplicationSurfaceOperation::CodeSignatureSearch => {
            evidence!(SymbolGraphPage<SymbolPrimitiveRecord>)
        }
        ApplicationSurfaceOperation::CodeImplementations
        | ApplicationSurfaceOperation::CodeCallers => {
            evidence!(SymbolGraphPage<SymbolRelationRecord>)
        }
        ApplicationSurfaceOperation::CodeTypeHierarchy => {
            evidence!(SymbolGraphPage<TypeHierarchyRecord>)
        }
        ApplicationSurfaceOperation::CodeCallees => {
            evidence!(CodeQueryPage<SymbolRelationRecord>)
        }
        ApplicationSurfaceOperation::CodeFacets => evidence!(CodeQueryPage<CodeFacetRecord>),
        ApplicationSurfaceOperation::CodeTimeline => evidence!(CodeQueryPage<CodeTimelineRecord>),
        ApplicationSurfaceOperation::CodeDeclaration
        | ApplicationSurfaceOperation::CodeDefinition
        | ApplicationSurfaceOperation::CodeTypeDefinition => {
            evidence!(CodeQueryPage<SymbolPrimitiveRecord>)
        }
        ApplicationSurfaceOperation::CodeReferences => {
            evidence!(CodeQueryPage<SymbolRelationRecord>)
        }
        ApplicationSurfaceOperation::SessionLookup => evidence!(SessionLookupResult),
        ApplicationSurfaceOperation::QualifiedName => evidence!(QualifiedNamePrimitiveResult),
        ApplicationSurfaceOperation::CallChain => evidence!(CallChainPrimitiveResult),
        ApplicationSurfaceOperation::FileDependents => evidence!(FileDependentsPrimitiveResult),
        ApplicationSurfaceOperation::SourceLines => evidence!(SourceLinesResult),
        ApplicationSurfaceOperation::SourceBody => evidence!(SourceBodyPrimitiveResult),
        ApplicationSurfaceOperation::SourceOutline => evidence!(SourceOutlinePrimitiveResult),
        ApplicationSurfaceOperation::ModuleApi => evidence!(ModuleApiPrimitiveResult),
        ApplicationSurfaceOperation::FileMetadata => evidence!(FileMetadataPrimitiveResult),
        ApplicationSurfaceOperation::HealthRead => evidence!(HealthReadResult),
        ApplicationSurfaceOperation::HealthDelta => evidence!(HealthDeltaResult),
        ApplicationSurfaceOperation::StorageStatus => evidence!(StorageStatusPrimitiveResult),
        ApplicationSurfaceOperation::DiagnosticsRead => evidence!(DiagnosticsPrimitiveResult),
        ApplicationSurfaceOperation::ConfigurationList
        | ApplicationSurfaceOperation::ConfigurationExplain
        | ApplicationSurfaceOperation::ConfigurationGet
        | ApplicationSurfaceOperation::ConfigurationSet
        | ApplicationSurfaceOperation::ConfigurationUnset
        | ApplicationSurfaceOperation::ConfigurationBatch
        | ApplicationSurfaceOperation::ConfigurationWriteCredential
        | ApplicationSurfaceOperation::ConfigurationObservedState
        | ApplicationSurfaceOperation::ConfigurationProtectedPreview
        | ApplicationSurfaceOperation::ConfigurationProtectedApply
        | ApplicationSurfaceOperation::ConfigurationRollbackPreview
        | ApplicationSurfaceOperation::ConfigurationRollbackApply
        | ApplicationSurfaceOperation::ConfigurationAudit
        | ApplicationSurfaceOperation::ConfigurationReset => {
            validate_configuration_outcome(operation, outcome)
        }
        ApplicationSurfaceOperation::ContextScoutStatus => evidence!(ContextScoutStatusV1),
        ApplicationSurfaceOperation::ContextScoutRecent => evidence!(ContextScoutRecentStateV1),
        ApplicationSurfaceOperation::ContextScoutExplain => evidence!(ContextScoutExplanationV1),
        ApplicationSurfaceOperation::ContextScoutCapability => {
            evidence!(ContextScoutCapabilityStateV1)
        }
        ApplicationSurfaceOperation::ContextScoutBudget => evidence!(ContextScoutBudgetStateV1),
        ApplicationSurfaceOperation::ContextScoutPause
        | ApplicationSurfaceOperation::ContextScoutResume => {
            effect_decodes::<ConfigurationMutationReceipt>(outcome)
        }
        ApplicationSurfaceOperation::ContextScoutCancel
        | ApplicationSurfaceOperation::ContextScoutDelivery
        | ApplicationSurfaceOperation::ContextScoutFeedback => {
            evidence!(ContextScoutMutationResultV1)
        }
        ApplicationSurfaceOperation::ContextScoutClaim => evidence!(ContextScoutClaimResultV1),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use tracedecay_api::{http_route_documents, openapi_document};
    use tracedecay_application::ApplicationWireOperation;
    use tracedecay_tool_catalog::{ProfileId, ScopeDimension};

    use super::{SettingSummary, build_application_wire_schema_registry, payload_decodes};

    #[test]
    fn list_payload_is_checked_against_the_concrete_result_type() {
        assert!(payload_decodes::<Vec<SettingSummary>>(Some(
            &serde_json::json!([])
        )));
        assert!(!payload_decodes::<Vec<SettingSummary>>(Some(
            &serde_json::json!({})
        )));
    }

    #[test]
    fn every_canonical_operation_binding_resolves_concrete_schema_bodies() {
        let catalog = super::super::application_surface_catalog_ref().unwrap();
        let registry = build_application_wire_schema_registry(catalog).unwrap();

        for operation in ApplicationWireOperation::ALL {
            let manifest = catalog
                .capabilities()
                .find(|manifest| {
                    manifest.binding_ids().iter().any(|binding_id| {
                        catalog.binding(binding_id).is_some_and(|binding| {
                            binding.operation().as_str() == operation.as_str()
                        })
                    })
                })
                .unwrap();
            for binding_id in manifest.binding_ids() {
                let schema = registry.get(binding_id).unwrap();
                assert_eq!(schema.operation(), operation);
                assert_eq!(schema.capability_id(), manifest.capability_id());
                assert_eq!(schema.binding_id(), binding_id);
                assert_eq!(schema.request().schema_ref(), manifest.request_schema());
                assert_eq!(schema.result().schema_ref(), manifest.result_schema());
            }
        }
    }

    #[test]
    fn authorized_http_openapi_uses_the_binding_keyed_wire_registry() {
        let catalog = super::super::application_surface_catalog_ref().unwrap();
        let registry = build_application_wire_schema_registry(catalog).unwrap();
        let authorized = catalog
            .capabilities()
            .map(|capability| capability.capability_id().clone())
            .collect();
        let scope = BTreeSet::from([
            ScopeDimension::Project,
            ScopeDimension::Repository,
            ScopeDimension::Worktree,
            ScopeDimension::Branch,
            ScopeDimension::Session,
            ScopeDimension::Resource,
        ]);
        let routes = http_route_documents(
            catalog,
            &ProfileId::new("profile.default").unwrap(),
            &authorized,
            &scope,
            &BTreeSet::new(),
            1,
        );

        assert!(!routes.is_empty());
        assert!(openapi_document(&routes, &registry).is_ok());
    }
}
