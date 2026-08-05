//! Daemon-owned native integration transaction coordinator.

mod gix_adapter;
mod transaction;

pub use gix_adapter::GixNativeIntegrationAdapter;
pub use transaction::{
    NativeApplyEffectV1, NativeIntegrationAuthorizationOutcomeV1,
    NativeIntegrationAuthorizationPort, NativeIntegrationMechanics, NativeIntegrationProbeV1,
    NativeIntegrationTransactionCoordinator,
};

#[cfg(test)]
#[path = "transaction_tests.rs"]
mod tests;
