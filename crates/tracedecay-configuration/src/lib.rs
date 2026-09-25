//! Transport-neutral configuration control plane and runtime pin surfaces.
//!
//! Retrieval-profile evaluation stays in `tracedecay-application::config::retrieval`
//! because it is production-load-bearing on search-eval. This crate must not
//! depend on `tracedecay-semantic` or `tracedecay-search-eval`.

pub mod config;
pub mod configuration;
#[cfg(any(test, feature = "test-helpers"))]
#[doc(hidden)]
pub mod test_support;

pub use config::model::{
    MIN_AUTO_TRACK_PR_POLL_SECS, RetentionConfig, SyncConfig, TelemetryConfig, brand_env,
    is_generated_path_segment, is_in_gitignore, resolve_path, resolve_path_with_discovery,
};
pub use config::{
    OpenedRuntimeConfiguration, PinnedRuntimeConfiguration, PinnedRuntimeConfigurationCachePort,
    RuntimeConfigurationTarget, cached_pinned_runtime_configuration,
    install_pinned_runtime_configuration_cache, lcm_summarizer_executables_for_project,
    publish_pinned_runtime_configuration,
};
pub use configuration::{
    ConfigurationControlPlane, ConfigurationControlPlaneOperations,
    ConfigurationMutationGrantAuthority, ConfigurationMutationGrantAuthorityError,
    ConfigurationMutationGrantAuthorityFuture, PolicyBackedConfigurationMutationAuthorization,
    ProductionConfigurationDaemonClient, ProductionUserSettingsDaemonClient,
    ProjectConfigurationRuntime, UserSettingsAuthorityError, UserSettingsDaemonClient,
    UserSettingsMutationPlanV1, UserSettingsMutationV1, UserSettingsSnapshotV1,
    commit_profile_code_index_worker_selection, map_profile_worker_configuration_error,
    parse_duration_millis, plan_user_settings_mutation, profile_code_index_worker_mutation,
};
