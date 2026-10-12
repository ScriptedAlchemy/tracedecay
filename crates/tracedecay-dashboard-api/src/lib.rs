//! `tracedecay dashboard`, local HTTP server for the dashboard UIs.
//!
//! Serves TraceDecay's embedded dashboard and its project-scoped JSON APIs:
//!
//! - `/api/plugins/holographic/*`  → canonical project facts, verified Grafeo
//!   topology, and deterministic FHRR projections derived on read
//! - `/api/plugins/hermes-lcm/*`   → LCM session store
//!   (`lcm_raw_messages` / `session_summary_nodes` in the resolved active project
//!   store where transcript ingest writes; see [`state::resolve_lcm_store`] for the
//!   fail-closed authority selection)
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

/// Installs the registered global/session schema into the kernel's fail-closed
/// port for this crate's test process.
///
/// `Database::publish_test_runtime` materialises a profile-scoped sidecar shard
/// that the kernel initialises through
/// `tracedecay_runtime_core::ports::registered_schema`. That port fails closed
/// until the real schema, owned by `tracedecay-global-db`, is registered.
/// Production wires it from the daemon composition root; this crate's test
/// target reuses the identical installer through its `test-helpers`
/// dev-dependency. Idempotent: the port keeps the first registration, so every
/// fixture entry point can call it unconditionally.
///
/// Fixtures built on `tracedecay_global_db::tests::harness` register the
/// installer themselves; only fixtures that reach `publish_test_runtime`
/// directly need this call.
#[cfg(test)]
pub(crate) fn register_test_schema_installer() {
    tracedecay_global_db::register_registered_schema_installer();
}

/// Fixtures for states this crate's tests build without a project runtime.
#[cfg(test)]
pub(crate) mod test_support {
    use std::path::Path;

    use tracedecay_automation_runtime::automation::host_io::{
        HostIo, ManagedSkillExportReport, PluginFile,
    };
    use tracedecay_domain::errors::Result;

    /// A host I/O bundle with no agent hosts behind it: writes land on disk
    /// plainly, export sweeps report nothing, and the managed-agent bundle is
    /// empty.
    pub(crate) fn fixture_host_io() -> HostIo {
        fn export_to_agents(_: &Path, _: &Path) -> Vec<ManagedSkillExportReport> {
            Vec::new()
        }

        fn export_to_agent_hosts(_: &Path, _: &Path, _: &Path) -> Vec<ManagedSkillExportReport> {
            Vec::new()
        }

        fn write_text(path: &Path, contents: &str) -> Result<()> {
            Ok(std::fs::write(path, contents)?)
        }

        fn write_json(path: &Path, value: &serde_json::Value) -> Result<()> {
            Ok(std::fs::write(path, serde_json::to_vec_pretty(value)?)?)
        }

        fn remove_host_file(path: &Path) -> std::io::Result<()> {
            std::fs::remove_file(path)
        }

        fn codex_agent_files() -> &'static [PluginFile] {
            &[]
        }

        HostIo {
            export_to_agents,
            export_to_agent_hosts,
            write_text,
            write_json,
            remove_host_file,
            codex_agent_files,
        }
    }
}

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

pub mod http;
pub mod routes;
pub mod state;
#[cfg(feature = "test-transport")]
pub mod test_serve;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod authority_tests;
