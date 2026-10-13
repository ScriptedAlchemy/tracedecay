//! `tracedecay dashboard`, local HTTP server for the dashboard UIs.
//!
//! Serves TraceDecay's embedded dashboard and its project-scoped JSON APIs:
//!
//! - `/api/plugins/holographic/*`  → canonical project facts, verified Grafeo
//!   topology, and deterministic MAP-C projections derived on read
//! - `/api/plugins/hermes-lcm/*`   → LCM session store
//!   (`lcm_raw_messages` / `session_summary_nodes` in the resolved active project
//!   store where transcript ingest writes; the daemon-registered session store
//!   is the only authority, and its absence fails closed)
//!
//! `/api/capabilities` advertises which current TraceDecay authorities are
//! mounted for the selected project.

use tracedecay_application as application;
pub use tracedecay_application::git_query;
pub use tracedecay_contracts::request_identity;
pub(crate) use tracedecay_graph_query as graph;
pub mod tracedecay;
// Crate-root re-exports the composition root reaches through its
// `crate::dashboard::*` shim: the application-surface injection contract and
// the dashboard-facing project runtime trait.
pub use application_surface::{
    DashboardApplicationRouters, DashboardApplicationRuntime, DashboardConfigurationApplyError,
    DashboardConfigurationApplyFuture, DashboardDaemonReadUnavailableV1,
    DashboardNativeIntegrationStatusFuture, DashboardScopeSetReadFuture,
};
pub use tracedecay::DashboardProjectContext;

mod access;
pub use access::DashboardAccessToken;
pub mod analytics_api;
pub mod application_surface;
mod automation_authority;
pub use automation_authority::{
    DashboardAutomationAuthorityErrorV1, DashboardAutomationAuthorityV1,
    DashboardAutomationRunFutureV1, DashboardAutomationRunInvocationV1,
    DashboardAutomationRunOutcomeV1, DashboardAutomationRunPortV1, DashboardAutomationRunRequestV1,
    DashboardManagedSkillCommandFutureV1, DashboardManagedSkillCommandInvocationV1,
    DashboardManagedSkillCommandOutcomeV1, DashboardManagedSkillCommandPortV1,
    DashboardManagedSkillCommandV1,
};
pub(crate) use automation_authority::{
    automation_authority_error_response, exact_automation_authority,
};
mod automation_config_api;
mod automation_fact_receipts_api;
mod automation_jobs_api;
mod automation_outcomes_api;
mod automation_run_api;
mod automation_run_service;
pub use automation_run_service::{
    DashboardAutomationWriter, standalone_dashboard_automation_writer,
};
mod automation_scheduler_api;
mod automation_skills_api;
pub mod cloud;
mod code_diagnostics_api;
pub mod code_index_freshness_api;
pub mod code_read_api;
pub mod config;
#[doc(hidden)]
pub mod contract_schema;
mod delivery_agent_usage;
mod delivery_api;
pub use delivery_api::{
    DashboardDeliveryProjectV1, DashboardDeliveryReadFutureV1, DashboardDeliveryReadPortV1,
    DashboardProximityAttentionReadFutureV1,
};
mod doctor_findings_api;
pub use doctor_findings_api::{DoctorFindingsPayloadV1, DoctorFindingsReadV1, doctor_findings};
mod events_api;
mod events_delivery;
mod explorer_api;
pub mod feedback_api;
mod graph_api;
mod graph_binding;
mod graph_service;
mod graph_structure_api;
mod lcm_api;
mod remote_status_api;
pub use lcm_api::{
    DashboardLcmCanonicalMatchesV1, DashboardLcmCanonicalMessageV1, DashboardLcmCanonicalPageV1,
    DashboardLcmCanonicalStatsV1, DashboardLcmCanonicalSummaryV1, DashboardLcmReadFutureV1,
    DashboardLcmReadOutcomeV1, DashboardLcmReadPortV1, DashboardLcmReadRequestV1,
    DashboardLcmReadStateV1, DashboardLcmTimelineBucketV1,
};
mod loom_api;
pub use loom_api::{
    DashboardGitCorrelationReadErrorV1, DashboardGitCorrelationReadFutureV1,
    DashboardGitCorrelationReadPortV1, DashboardGitCorrelationReadV1,
};
mod memory_analysis;
mod memory_api;
mod memory_service;
mod multi_root_api;
mod native_integration_api;
mod observe;
pub mod project_graph;
pub mod project_registry;
mod projects;
mod read_model;
mod request_deadline;
mod savings_api;
mod session_authority;
pub use session_authority::{
    DashboardSessionAuthoritiesV1, DashboardSessionAuthorityStateV1, DashboardSessionMountV1,
    DashboardSessionResolutionV1,
};
mod snapshot_cache;
use tracedecay_session_memory::provider_pricing as savings_pricing;
pub mod scope;
mod settings_api;
pub use settings_api::{
    DashboardCodeIndexWorkerConfigurationV1, DashboardCodeIndexWorkerSettingsCommitFuture,
    DashboardCodeIndexWorkerSettingsCommitV1, DashboardCodeIndexWorkerSettingsErrorV1,
    DashboardCodeIndexWorkerSettingsFuture, DashboardProfileCodeIndexWorkerSettingsPort,
    PrAutoTrackManagedSummaryEntryV1, PrAutoTrackManagedSummaryReader,
};
mod storage_telemetry_api;
mod token_count;
mod util;
mod work_api;

mod application_routes;
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod authority_tests;
mod capabilities;
mod http_admission;
mod project_gateway;
mod routes;
mod server;
mod state;
#[cfg(test)]
pub(crate) mod test_support;
#[cfg(feature = "test-transport")]
mod test_transport;

pub use http_admission::{
    DashboardHttpRequestControlV1, RequestControl, RequestControlMissing,
    with_dashboard_http_admission,
};
pub use routes::router;
pub use server::{bind_dashboard, validate_dashboard_host};
pub use state::{
    AdmittedDoctorReportV1, AutomationSchedulerReconcileFuture, AutomationSchedulerReconciler,
    DashboardAutomationObservationFuture, DashboardAutomationObservationRecorderV1, DashboardState,
    DashboardStateCompositionV1, DoctorReportReadFuture, DoctorReportReader,
    RemoteOperationalStatusReader, build_selected_project_state,
    build_state_with_automation_reconciler, config_error, project_memory_owner, storage_mode_label,
};
#[cfg(feature = "test-transport")]
pub use test_transport::{
    DashboardHostAdmissionTestAuthorityV1, DashboardTestEndpointV1, DashboardTestProjectGraphsV1,
    run_until_shutdown_for_tests_with_host_admission,
};

/// Default port for `tracedecay dashboard`; override with `--port`.
pub const DEFAULT_PORT: u16 = 7341;

#[cfg(test)]
pub(crate) use test_support::register_test_schema_installer;
