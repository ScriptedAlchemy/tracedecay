//! Transport-neutral PR11 configuration control plane.

pub mod authorization;
pub mod operations;
pub mod ports;
pub mod types;

pub use authorization::{
    ConfigurationMutationGrantAuthority, ConfigurationMutationGrantAuthorityError,
    PolicyBackedConfigurationMutationAuthorization,
};
pub use operations::{ConfigurationControlPlane, ConfigurationControlPlaneOperations};
pub use ports::{
    ConfigurationClock, ConfigurationControlStore, ConfigurationCurrentStateV1,
    ConfigurationMutationAuthorizationPort, CredentialWritePort,
    CurrentConfigurationMutationAuthorizationV1, ScopeResolutionPort, ScopeRevalidationEvidenceV1,
};
pub use types::{
    AuthorizedActor, ComponentConfigurationState, ConfigurationAuditPage, ConfigurationAuditQuery,
    ConfigurationError, ConfigurationMutationAuthority, ConfigurationMutationReceipt,
    ConfigurationPlanContext, ConfigurationRollbackRequest, DirectConfigurationMutation,
    ResolvedSetting, SettingSummary, WriteOnlyCredentialMutation,
};
