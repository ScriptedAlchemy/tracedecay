//! Transport-neutral configuration control plane and runtime pin surfaces.
//!
//! Retrieval-profile evaluation stays in `tracedecay-application::config::retrieval`
//! because it is production-load-bearing on search-eval. This crate must not
//! depend on `tracedecay-semantic` or `tracedecay-search-eval`.

pub mod config;
pub mod configuration;

pub use config::model::{
    CONFIG_FILENAME, MIN_AUTO_TRACK_PR_POLL_SECS, RetentionConfig, SYNC_RETENTION_SETTING_KEY,
    SyncConfig, TelemetryConfig, TraceDecayConfig, brand_env, get_config_path, is_excluded,
    is_excluded_dir, is_generated_path_segment, is_in_gitignore, is_included, load_config,
    load_config_from_path, resolve_path, resolve_path_with_discovery, save_config_to_path,
};
pub use config::{
    OpenedRuntimeConfiguration, PinnedRuntimeConfiguration, PinnedRuntimeConfigurationCachePort,
    RuntimeConfigurationTarget, cached_pinned_runtime_configuration,
    install_pinned_runtime_configuration_cache, publish_pinned_runtime_configuration,
};
pub use configuration::{
    AuthorizedActor, CONFIGURATION_AUDIT_PAGE_LIMIT, ComponentConfigurationState,
    ConfigurationAuditPage, ConfigurationAuditQuery, ConfigurationClock, ConfigurationControlPlane,
    ConfigurationControlPlaneOperations, ConfigurationControlStore, ConfigurationCurrentStateV1,
    ConfigurationError, ConfigurationMutationAuthority, ConfigurationMutationAuthorizationPort,
    ConfigurationMutationGrantAuthority, ConfigurationMutationGrantAuthorityError,
    ConfigurationMutationGrantAuthorityFuture, ConfigurationMutationReceipt,
    ConfigurationOperationFuture, ConfigurationPlanContext, ConfigurationRollbackRequest,
    ConfigurationSettlementAuthorityV1, CurrentConfigurationMutationAuthorizationV1,
    DirectConfigurationMutation, PolicyBackedConfigurationMutationAuthorization,
    ProductionConfigurationDaemonClient, ProductionUserSettingsDaemonClient,
    ProjectConfigurationRuntime, ResolvedSetting, ScopeResolutionPort, ScopeRevalidationEvidenceV1,
    SettingSummary, UserSettingsAuthorityError, UserSettingsDaemonClient,
    UserSettingsMutationPlanV1, UserSettingsMutationV1, UserSettingsSnapshotV1,
    commit_profile_code_index_worker_selection, configuration_layer_scope_digest,
    map_profile_worker_configuration_error, parse_duration_millis, plan_user_settings_mutation,
    profile_code_index_worker_mutation,
};
