//! Daemon-owned native integration transaction coordinator.

mod analysis;
mod authorization;
mod gix_adapter;
mod status_broadcast;
mod topology;
mod transaction;

pub use analysis::{
    NativeIntegrationAnalysisPort, NativeIntegrationGenerationAnalysisV1,
    analyze_native_integration_generations, native_integration_generation_binding,
};
pub use authorization::{
    DaemonNativeIntegrationAuthorization, NativeIntegrationAuthorizationError,
};
pub use gix_adapter::GixNativeIntegrationAdapter;
pub use status_broadcast::NativeIntegrationStatusBroadcastV1;
pub use topology::{ExactPairNativeIntegrationTopology, NativeIntegrationGraphRuntimeProviderV1};
pub use transaction::{
    NativeApplyEffectV1, NativeIntegrationAnalysisRevalidationV1,
    NativeIntegrationAuthorizationOutcomeV1, NativeIntegrationAuthorizationPort,
    NativeIntegrationMechanics, NativeIntegrationProbeV1, NativeIntegrationTransactionCoordinator,
};

use tracedecay_contracts::NativeIntegrationPortError;

/// Maps any native-side failure into the port's opaque native variant.
///
/// Shared by every adapter in this module so one failure taxonomy reaches the
/// application boundary; the concrete Git or domain error text stays a
/// diagnostic, never an authorization or control signal.
fn native_error(error: impl std::fmt::Display) -> NativeIntegrationPortError {
    NativeIntegrationPortError::Native(error.to_string())
}

fn domain_error(error: tracedecay_domain::DomainError) -> NativeIntegrationPortError {
    NativeIntegrationPortError::Native(error.to_string())
}

#[cfg(test)]
#[path = "transaction_tests.rs"]
mod tests;
