//! Transport-neutral configuration control plane.

pub mod authorization;
pub mod operations;
pub mod ports;
pub mod profile_workers;
pub mod runtime;
pub mod types;
pub mod user_settings;

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
pub use profile_workers::{
    commit_profile_code_index_worker_selection, map_profile_worker_configuration_error,
    profile_code_index_worker_mutation,
};
pub use runtime::{ProductionConfigurationDaemonClient, ProjectConfigurationRuntime};
pub use types::{
    AuthorizedActor, CONFIGURATION_AUDIT_PAGE_LIMIT, ComponentConfigurationState,
    ConfigurationAuditPage, ConfigurationAuditQuery, ConfigurationError,
    ConfigurationMutationAuthority, ConfigurationMutationReceipt, ConfigurationPlanContext,
    ConfigurationRollbackRequest, ConfigurationSettlementAuthorityV1, CredentialWriteHandleV1,
    DirectConfigurationMutation, ResolvedSetting, SettingSummary, WriteOnlyCredentialMutation,
    configuration_layer_scope_digest,
};
pub use user_settings::{
    ProductionUserSettingsDaemonClient, UserSettingsAuthorityError, UserSettingsDaemonClient,
    UserSettingsMutationPlanV1, UserSettingsMutationV1, UserSettingsSnapshotV1,
    parse_duration_millis, plan_user_settings_mutation,
};
