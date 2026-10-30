//! Dashboard context derived from a registered project graph.

use tracedecay_dashboard_api::DashboardProjectContext;

/// The dashboard context of `graph`, owned by `profile` when a daemon serves
/// it, else by the profile the graph was opened in. User settings resolve from
/// `profile_sessions`, the owner's registered profile store; without one they
/// are unavailable.
pub fn dashboard_project_context(
    graph: &crate::project::TraceDecay,
    profile: Option<&tracedecay_runtime_core::config::ProfileRoot>,
    profile_sessions: Option<&tracedecay_global_db::RegisteredGlobalDbLeaseV1>,
) -> tracedecay_domain::errors::Result<DashboardProjectContext> {
    let profile = match profile {
        Some(profile) => profile.clone(),
        None => tracedecay_runtime_core::config::ProfileRoot::new(graph.profile_root()?),
    };
    let user_settings_client = match profile_sessions {
        Some(database) => tracedecay_configuration::ProductionUserSettingsDaemonClient::new(
            database.clone(),
            database.binding().shard_id.profile_id.clone(),
            profile.data_dir().to_path_buf(),
        ),
        None => tracedecay_configuration::ProductionUserSettingsDaemonClient::default(),
    };
    Ok(DashboardProjectContext {
        profile,
        store_layout: graph.store_layout().clone(),
        dashboard_db_path: graph.dashboard_db_path(),
        dashboard_database: graph.dashboard_database_guard(),
        retention_config: graph.get_config().sync.retention.clone(),
        host_io: tracedecay_agent_hosts::host_io(),
        user_settings_client: std::sync::Arc::new(user_settings_client),
    })
}
