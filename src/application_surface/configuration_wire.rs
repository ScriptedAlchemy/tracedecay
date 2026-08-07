use serde::de::DeserializeOwned;
use serde_json::Value;
use tracedecay_application::{
    ApplicationOutcome, ApplicationWireOperation, ApplicationWireSchemaRegistryV1,
    ApplicationWireSchemaV1, CancellationStage, ComponentConfigurationState,
    ConfigurationAuditPage, ConfigurationMutationReceipt, OperationTermination, ResolvedSetting,
    SettingSummary,
    configuration_surface_catalog_contribution, configuration_surface_operation,
};
use tracedecay_domain::configuration::{CredentialReferenceMetadataV1, ProtectedChangePlan};
use tracedecay_tool_catalog::{
    CancellationContract, CancellationPoint, CatalogSnapshotV1, ReceiptContract,
    ReconciliationContract, TerminalState, TerminalStateContract,
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
    )
}

pub(super) fn build_configuration_wire_schema_registry(
    catalog: &CatalogSnapshotV1,
) -> Result<ApplicationWireSchemaRegistryV1, ApplicationSurfaceAdapterError> {
    let contribution = configuration_surface_catalog_contribution()?;
    let mut schemas = Vec::new();
    for name in tracedecay_application::configuration::CONFIGURATION_SURFACE_OPERATION_NAMES {
        let operation = ApplicationWireOperation::from_catalog_name(name)
            .ok_or(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized)?;
        let application_operation = configuration_surface_operation(name)?
            .ok_or(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized)?;
        let manifest = catalog
            .capability(application_operation.capability_id())
            .ok_or(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized)?;
        let authority = contribution
            .executable_schema(manifest.capability_id())
            .ok_or(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized)?;
        for binding_id in manifest.binding_ids() {
            let binding = catalog
                .binding(binding_id)
                .ok_or(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized)?;
            schemas.push(ApplicationWireSchemaV1::from_catalog(
                operation,
                manifest,
                binding,
                authority.request_schema().clone(),
                authority.result_schema().clone(),
            )?);
        }
    }
    ApplicationWireSchemaRegistryV1::new(schemas).map_err(Into::into)
}

fn payload_decodes<T: DeserializeOwned>(payload: Option<&Value>) -> bool {
    payload.is_none_or(|value| serde_json::from_value::<T>(value.clone()).is_ok())
}

fn configuration_terminal_is_legal(
    termination: OperationTermination,
    terminal_states: &TerminalStateContract,
) -> bool {
    let terminal_state = match termination {
        OperationTermination::Completed => TerminalState::Completed,
        OperationTermination::Cancelled => TerminalState::Cancelled,
        OperationTermination::TimedOut => TerminalState::TimedOut,
        OperationTermination::Failed => TerminalState::Failed,
        OperationTermination::Unavailable => TerminalState::Unavailable,
        OperationTermination::Partial => TerminalState::Partial,
        OperationTermination::EffectUnknown => TerminalState::EffectUnknown,
    };
    terminal_states.contains(terminal_state)
}

fn configuration_cancellation_is_legal(
    outcome: &ApplicationOutcome<Value>,
    cancellation: &CancellationContract,
) -> bool {
    let observation = match outcome {
        ApplicationOutcome::Evidence(packet) => packet.execution.cancellation.as_ref(),
        ApplicationOutcome::Preview(preview) => preview.execution.cancellation.as_ref(),
        ApplicationOutcome::Effect(effect) => effect.execution.cancellation.as_ref(),
    };
    let Some(observation) = observation else {
        return true;
    };
    let point = match observation.stage {
        CancellationStage::BeforeAdmission => CancellationPoint::BeforeAdmission,
        CancellationStage::BeforeRead => CancellationPoint::BeforeRead,
        CancellationStage::DuringRead => CancellationPoint::DuringRead,
        CancellationStage::BeforeEffect => CancellationPoint::BeforeEffect,
        CancellationStage::EffectInFlight => CancellationPoint::EffectInFlight,
        CancellationStage::Reconciling => CancellationPoint::Reconciling,
        CancellationStage::AfterCommit => CancellationPoint::AfterCommit,
    };
    cancellation.observes(point)
}

/// Validate the transport serialization carrier against the concrete result
/// DTO before an adapter can publish it.
pub(super) fn validate_configuration_outcome(
    operation: ApplicationSurfaceOperation,
    outcome: &ApplicationOutcome<Value>,
    cancellation: &CancellationContract,
    terminal_states: &TerminalStateContract,
    receipt: ReceiptContract,
    reconciliation: ReconciliationContract,
) -> bool {
    let termination = match outcome {
        ApplicationOutcome::Evidence(packet) => packet.execution.termination,
        ApplicationOutcome::Preview(preview) => preview.execution.termination,
        ApplicationOutcome::Effect(effect) => effect.execution.termination,
    };
    let lifecycle_shape_is_legal = matches!(
        (receipt, reconciliation, outcome),
        (
            ReceiptContract::Operation,
            ReconciliationContract::NotRequired,
            ApplicationOutcome::Evidence(_) | ApplicationOutcome::Preview(_)
        ) | (
            ReceiptContract::DurableEffect,
            ReconciliationContract::Required,
            ApplicationOutcome::Effect(_)
        )
    );
    if !lifecycle_shape_is_legal
        || !configuration_cancellation_is_legal(outcome, cancellation)
        || !configuration_terminal_is_legal(termination, terminal_states)
    {
        return false;
    }
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
        ApplicationWireOperation, OperationTermination,
        configuration::CONFIGURATION_SURFACE_OPERATION_NAMES, configuration_surface_operation,
    };

    use super::{
        SettingSummary, build_configuration_wire_schema_registry, configuration_terminal_is_legal,
        payload_decodes,
    };

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

    #[test]
    fn configuration_terminals_are_checked_against_the_owning_manifest() {
        let catalog = super::super::application_surface_catalog_ref().unwrap();
        let set = configuration_surface_operation("configuration_set")
            .unwrap()
            .unwrap();
        let set_terminals = catalog
            .capability(set.capability_id())
            .unwrap()
            .terminal_states();
        assert!(configuration_terminal_is_legal(
            OperationTermination::EffectUnknown,
            set_terminals
        ));
        assert!(!configuration_terminal_is_legal(
            OperationTermination::Cancelled,
            set_terminals
        ));

        let list = configuration_surface_operation("configuration_list")
            .unwrap()
            .unwrap();
        let list_terminals = catalog
            .capability(list.capability_id())
            .unwrap()
            .terminal_states();
        assert!(configuration_terminal_is_legal(
            OperationTermination::Cancelled,
            list_terminals
        ));
        assert!(!configuration_terminal_is_legal(
            OperationTermination::EffectUnknown,
            list_terminals
        ));
    }
}
