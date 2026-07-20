//! Transport-neutral PR11 configuration control plane.

pub mod operations;
pub mod ports;
pub mod types;

pub use operations::{ConfigurationControlPlane, ConfigurationControlPlaneOperations};
pub use ports::{
    ConfigurationClock, ConfigurationControlStore, ConfigurationCurrentStateV1,
    CredentialWritePort, ScopeResolutionPort, ScopeRevalidationEvidenceV1,
};
pub use types::{
    AuthorizedActor, ComponentConfigurationState, ConfigurationAuditPage, ConfigurationAuditQuery,
    ConfigurationError, ConfigurationMutationReceipt, ConfigurationPlanContext,
    ConfigurationRollbackRequest, DirectConfigurationMutation, ResolvedSetting, SettingSummary,
    WriteOnlyCredentialMutation,
};
