//! Transport-neutral configuration control plane and runtime pin surfaces.
//!
//! Retrieval-profile evaluation stays in `tracedecay-usecases::config::retrieval`
//! because it is production-load-bearing on search-eval. This crate must not
//! depend on `tracedecay-semantic` or `tracedecay-search-eval`.

pub mod config;
pub mod configuration;

pub use config::{
    OpenedRuntimeConfiguration, PinnedRuntimeConfiguration, PinnedRuntimeConfigurationCachePort,
    RuntimeConfigurationAuthorityPort, RuntimeConfigurationFuture, RuntimeConfigurationTarget,
    SyncConfig, TelemetryConfig, TraceDecayConfig, cached_pinned_runtime_configuration,
    install_pinned_runtime_configuration_cache, install_runtime_configuration_authority,
    open_runtime_configuration_for_registered_database,
    open_runtime_configuration_for_registered_database_read_only,
    publish_pinned_runtime_configuration,
};
pub use configuration::{
    AuthorizedActor, CONFIGURATION_AUDIT_PAGE_LIMIT, ComponentConfigurationState,
    ConfigurationAuditPage, ConfigurationAuditQuery, ConfigurationClock, ConfigurationControlPlane,
    ConfigurationControlPlaneOperations, ConfigurationControlStore, ConfigurationCurrentStateV1,
    ConfigurationError, ConfigurationMutationAuthority, ConfigurationMutationAuthorizationPort,
    ConfigurationMutationGrantAuthority, ConfigurationMutationGrantAuthorityError,
    ConfigurationMutationGrantAuthorityFuture, ConfigurationMutationReceipt,
    ConfigurationOperationFuture, ConfigurationPlanContext, ConfigurationRollbackRequest,
    ConfigurationSettlementAuthorityV1, CredentialWriteHandleV1, CredentialWritePort,
    CurrentConfigurationMutationAuthorizationV1, DirectConfigurationMutation,
    PolicyBackedConfigurationMutationAuthorization, ProductionConfigurationDaemonClient,
    ProductionUserSettingsDaemonClient, ProjectConfigurationRuntime, ResolvedSetting,
    ScopeResolutionPort, ScopeRevalidationEvidenceV1, SettingSummary, UserSettingsAuthorityError,
    UserSettingsDaemonClient, UserSettingsMutationPlanV1, UserSettingsMutationV1,
    UserSettingsSnapshotV1, WriteOnlyCredentialMutation,
    commit_profile_code_index_worker_selection, configuration_layer_scope_digest,
    map_profile_worker_configuration_error, parse_duration_millis, plan_user_settings_mutation,
    profile_code_index_worker_mutation,
};
