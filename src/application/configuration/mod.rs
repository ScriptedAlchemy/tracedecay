//! Transport-neutral PR11 configuration control plane.

pub mod authorization;
mod ephemeral_grants;
pub mod operations;
pub mod ports;
pub mod runtime;
pub mod types;

pub use authorization::{
    ConfigurationMutationGrantAuthority, ConfigurationMutationGrantAuthorityError,
    ConfigurationMutationGrantAuthorityFuture, PolicyBackedConfigurationMutationAuthorization,
};
pub use operations::{ConfigurationControlPlane, ConfigurationControlPlaneOperations};
pub use ports::{
    ConfigurationClock, ConfigurationControlStore, ConfigurationCurrentStateV1,
    ConfigurationMutationAuthorizationPort, ConfigurationOperationFuture, CredentialWritePort,
    CurrentConfigurationMutationAuthorizationV1, ScopeResolutionPort, ScopeRevalidationEvidenceV1,
};
pub use runtime::{ProductionConfigurationDaemonClient, ProjectConfigurationRuntime};
pub use types::{
    AuthorizedActor, CONFIGURATION_AUDIT_PAGE_LIMIT, ComponentConfigurationState,
    ConfigurationAuditPage, ConfigurationAuditQuery, ConfigurationError,
    ConfigurationMutationAuthority, ConfigurationMutationReceipt, ConfigurationPlanContext,
    ConfigurationRollbackRequest, CredentialWriteHandleV1, DirectConfigurationMutation,
    ResolvedSetting, SettingSummary, WriteOnlyCredentialMutation,
};
