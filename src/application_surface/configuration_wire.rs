use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tracedecay_application::{
    ApplicationOutcome, ApplicationWireOperation, ApplicationWireSchemaRegistryV1,
    ApplicationWireSchemaV1, ConfigurationAuditRequestV1, ConfigurationBatchRequestV1,
    ConfigurationGetRequestV1, ConfigurationListRequestV1, ConfigurationObservedStateRequestV1,
    ConfigurationProtectedApplyRequestV1, ConfigurationProtectedPreviewRequestV1,
    ConfigurationResetOutcomeV1, ConfigurationResetRequestV1, ConfigurationRollbackApplyRequestV1,
    ConfigurationRollbackPreviewRequestV1, ConfigurationSetRequestV1, ConfigurationUnsetRequestV1,
    ConfigurationWriteCredentialRequestV1, configuration_surface_operation,
};
use tracedecay_domain::configuration::{CredentialReferenceMetadataV1, ProtectedChangePlan};
use tracedecay_tool_catalog::{CatalogSnapshotV1, SchemaBodyAuthorityV1};
use tracedecay_usecases::configuration::{
    ComponentConfigurationState, ConfigurationAuditPage, ConfigurationMutationReceipt,
    ResolvedSetting, SettingSummary,
};

use super::{ApplicationSurfaceAdapterError, ApplicationSurfaceOperation};

pub(super) fn is_configuration_operation(operation: ApplicationSurfaceOperation) -> bool {
    matches!(
        operation,
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
            | ApplicationSurfaceOperation::ConfigurationReset
    )
}

fn add_schema<Request, Result>(
    catalog: &CatalogSnapshotV1,
    operation: ApplicationWireOperation,
    schemas: &mut Vec<ApplicationWireSchemaV1>,
) -> Result<(), ApplicationSurfaceAdapterError>
where
    Request: JsonSchema,
    Result: JsonSchema,
{
    let application_operation = configuration_surface_operation(operation.as_str())?
        .ok_or(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized)?;
    let manifest = catalog
        .capability(application_operation.capability_id())
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

pub(super) fn build_configuration_wire_schema_registry(
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
    ApplicationWireSchemaRegistryV1::new(schemas).map_err(Into::into)
}

fn payload_decodes<T: DeserializeOwned>(payload: Option<&Value>) -> bool {
    payload.is_none_or(|value| serde_json::from_value::<T>(value.clone()).is_ok())
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
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use tracedecay_application::{
        ApplicationWireOperation, configuration::CONFIGURATION_SURFACE_OPERATION_NAMES,
        configuration_surface_operation,
    };

    use super::{SettingSummary, build_configuration_wire_schema_registry, payload_decodes};

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
    fn configuration_catalog_bindings_resolve_concrete_schema_bodies() {
        let catalog = super::super::application_surface_catalog_ref().unwrap();
        let registry = build_configuration_wire_schema_registry(catalog).unwrap();

        for name in CONFIGURATION_SURFACE_OPERATION_NAMES {
            let operation = ApplicationWireOperation::from_catalog_name(name).unwrap();
            let application_operation = configuration_surface_operation(name).unwrap().unwrap();
            let manifest = catalog
                .capability(application_operation.capability_id())
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
}
