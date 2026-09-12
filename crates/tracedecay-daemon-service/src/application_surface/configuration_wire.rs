use serde::de::DeserializeOwned;
use serde_json::Value;
use tracedecay_contracts::{
    ApplicationOutcome, CancellationStage, ComponentConfigurationState, ConfigurationAuditPage,
    ConfigurationMutationReceipt, OperationTermination, ResolvedSetting, SettingSummary,
};
use tracedecay_domain::configuration::ProtectedChangePlan;
use tracedecay_tool_catalog::{
    ApplicationSurfaceOperation, BindingId, CancellationContract, CancellationPoint,
    CatalogContributionV1, CatalogSnapshotV1, ReceiptContract, ReconciliationContract,
    TerminalState, TerminalStateContract,
};

use tracedecay_daemon_protocol::ApplicationSurfaceAdapterError;

pub(super) const CONFIGURATION_WIRE_OPERATIONS: [ApplicationSurfaceOperation; 11] = [
    ApplicationSurfaceOperation::ConfigurationList,
    ApplicationSurfaceOperation::ConfigurationGet,
    ApplicationSurfaceOperation::ConfigurationSet,
    ApplicationSurfaceOperation::ConfigurationUnset,
    ApplicationSurfaceOperation::ConfigurationBatch,
    ApplicationSurfaceOperation::ConfigurationObservedState,
    ApplicationSurfaceOperation::ConfigurationProtectedPreview,
    ApplicationSurfaceOperation::ConfigurationProtectedApply,
    ApplicationSurfaceOperation::ConfigurationRollbackPreview,
    ApplicationSurfaceOperation::ConfigurationRollbackApply,
    ApplicationSurfaceOperation::ConfigurationAudit,
];

pub(super) fn is_configuration_operation(operation: ApplicationSurfaceOperation) -> bool {
    CONFIGURATION_WIRE_OPERATIONS.contains(&operation)
}

pub(super) fn configuration_binding_has_schema(
    catalog: &CatalogSnapshotV1,
    contribution: &CatalogContributionV1,
    binding_id: &BindingId,
) -> bool {
    let Some(binding) = catalog.binding(binding_id) else {
        return false;
    };
    let Some(manifest) = catalog.capability(binding.capability_id()) else {
        return false;
    };
    contribution
        .executable_schema(manifest.capability_id())
        .is_some_and(|authority| {
            authority.request_schema().schema_ref() == manifest.request_schema()
                && authority.result_schema().schema_ref() == manifest.result_schema()
        })
}

/// The application invocation payload for a configuration operation is the
/// operation's own request body — the same shape
/// `parse_application_surface_request` accepts from every caller surface — not
/// the `operation`/`request` envelope `ConfigurationWireRequestV1` uses to
/// carry it across the daemon contract. Sending the envelope made the executor
/// re-parse a tagged wrapper against a `deny_unknown_fields` request struct,
/// so every configuration read and write routed through the daemon invocation
/// executor failed admission as `InvalidRequest`.
#[hotpath::measure(label = "application_surface.configuration.payload")]
pub(super) fn configuration_invocation_payload(
    request: &tracedecay_contracts::ConfigurationWireRequestV1,
) -> Result<Value, ApplicationSurfaceAdapterError> {
    let mut wire = serde_json::to_value(request)
        .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)?;
    wire.get_mut("request")
        .map(Value::take)
        .ok_or(ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
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
        (ApplicationSurfaceOperation::ConfigurationGet, ApplicationOutcome::Evidence(packet)) => {
            payload_decodes::<ResolvedSetting>(packet.payload.as_ref())
        }
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
    use tracedecay_contracts::{
        OperationTermination, configuration::configuration_surface_operation_names,
        configuration_surface_operation,
    };

    use super::{
        CONFIGURATION_WIRE_OPERATIONS, SettingSummary, configuration_binding_has_schema,
        configuration_terminal_is_legal, payload_decodes,
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
    fn configuration_catalog_bindings_resolve_only_mounted_schema_bodies() {
        let catalog = super::super::application_surface_catalog_ref().unwrap();
        let contribution =
            tracedecay_contracts::configuration_surface_catalog_contribution().unwrap();

        for operation in CONFIGURATION_WIRE_OPERATIONS {
            let name = operation.as_str();
            let application_operation = configuration_surface_operation(name).unwrap().unwrap();
            let manifest = catalog
                .capability(application_operation.capability_id())
                .unwrap();
            for binding_id in manifest.binding_ids() {
                assert!(configuration_binding_has_schema(
                    catalog,
                    &contribution,
                    binding_id
                ));
            }
        }

        for name in configuration_surface_operation_names() {
            if CONFIGURATION_WIRE_OPERATIONS
                .iter()
                .any(|operation| operation.as_str() == name)
            {
                continue;
            }
            let application_operation = configuration_surface_operation(name).unwrap().unwrap();
            let manifest = catalog
                .capability(application_operation.capability_id())
                .unwrap();
            for binding_id in manifest.binding_ids() {
                assert!(!configuration_binding_has_schema(
                    catalog,
                    &contribution,
                    binding_id
                ));
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
