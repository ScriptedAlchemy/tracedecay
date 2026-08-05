use serde::de::DeserializeOwned;
use serde_json::Value;
use tracedecay_application::ApplicationOutcome;
use tracedecay_domain::configuration::{CredentialReferenceMetadataV1, ProtectedChangePlan};
use tracedecay_usecases::configuration::{
    ComponentConfigurationState, ConfigurationAuditPage, ConfigurationMutationReceipt,
    ResolvedSetting, SettingSummary,
};

use super::ApplicationSurfaceOperation;

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
    use super::{SettingSummary, payload_decodes};

    #[test]
    fn list_payload_is_checked_against_the_concrete_result_type() {
        assert!(payload_decodes::<Vec<SettingSummary>>(Some(
            &serde_json::json!([])
        )));
        assert!(!payload_decodes::<Vec<SettingSummary>>(Some(
            &serde_json::json!({})
        )));
    }
}
