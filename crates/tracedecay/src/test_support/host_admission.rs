//! Root MCP composition over the registered host-admission test runtime.
//!
//! The runtime itself — registered databases, session registry, and the
//! project-graph opens through it — lives in `tracedecay-project`; this
//! module keeps its historical path and adds the pieces that need the root's
//! MCP server: tool calls through the registry-aware dispatcher and direct
//! server construction contexts.

#[cfg(any(test, feature = "test-transport"))]
use std::sync::Arc;

use tracedecay_domain::errors::Result;
#[cfg(any(test, feature = "test-transport"))]
use tracedecay_domain::errors::TraceDecayError;
use tracedecay_sessions::admission::HostAdmissionScope;

pub use tracedecay_project::test_support::host_admission::{
    HostAdmissionDatabaseIdentityV1, HostAdmissionTestRuntimeV1,
    LcmExternalPayloadManifestTestRecord, LcmLineageCountsForTest, LcmLineageFaultForTest,
    ProjectScopedTestRuntimeV1, SessionTemporalFixtureCountV1, await_bound_graph_runtime,
    ensure_process_background_cpu_authority,
};

use crate::project::TraceDecay;
use tracedecay_mcp::handlers::mcp_session_authorities;

/// Calls one MCP tool through the registry-aware dispatcher with this
/// runtime's registered databases as the tool's authorities.
#[doc(hidden)]
pub async fn call_mcp_tool_for_test(
    runtime: &HostAdmissionTestRuntimeV1,
    cg: &TraceDecay,
    tool_name: &str,
    arguments: serde_json::Value,
    server_stats: Option<serde_json::Value>,
    scope_prefix: Option<&str>,
) -> Result<tracedecay_mcp::ToolResult> {
    let profile_database = runtime.profile_database_lease();
    let project_registry_reads =
        tracedecay_daemon_service::DaemonProjectRegistryReadService::new(profile_database.clone());
    crate::mcp::tools::handle_tool_call_with_registry_options(
        cg,
        tool_name,
        arguments,
        server_stats,
        scope_prefix,
        crate::mcp::tools::ToolCallRegistryOptions {
            global_db: Some(profile_database),
            project_registry_reads: Some(&project_registry_reads),
            accounting_db: Some(profile_database.as_ref()),
            registered_project_session_db: runtime
                .registered_database_arc(HostAdmissionScope::Project),
            registered_savings_db: Some(profile_database.clone()),
            profile_root: Some(runtime.profile_root_for_test()),
            session_authorities: mcp_session_authorities(runtime),
            ..Default::default()
        }
        .admit_opened_project(cg)?,
    )
    .await
}

/// A direct MCP server construction context bound to this runtime's
/// registered databases, profile identity, and background CPU authority.
#[cfg(any(test, feature = "test-transport"))]
pub(crate) fn mcp_server_context_for_test(
    runtime: Arc<HostAdmissionTestRuntimeV1>,
    cg: TraceDecay,
    scope_prefix: Option<String>,
) -> Result<crate::mcp::server::McpServerConstructionContext> {
    let profile_root = runtime.profile_root_for_test().to_path_buf();
    let project_sessions = runtime
        .registered_database_arc(HostAdmissionScope::Project)
        .ok_or_else(|| TraceDecayError::Database {
            operation: "bind MCP test project sessions".to_owned(),
            message: "registered ProjectSessions mount is unavailable".to_owned(),
        })?;
    let profile_sessions = runtime
        .registered_database_arc(HostAdmissionScope::Profile)
        .ok_or_else(|| TraceDecayError::Database {
            operation: "bind MCP test profile sessions".to_owned(),
            message: "registered ProfileSessions mount is unavailable".to_owned(),
        })?;
    let profile_database = runtime.profile_database_lease().clone();
    let profile_identity =
        tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)?;
    let mut context = crate::mcp::server::McpServerConstructionContext::direct(cg, scope_prefix)
        .with_direct_databases(
            Some(profile_database.clone()),
            Some(profile_database),
            Some(project_sessions),
            Some(profile_sessions),
        );
    context.profile_root = Some(profile_root);
    context.profile_identity = Some(Arc::new(profile_identity));
    context.background_cpu = Some(runtime.background_cpu());
    context.host_admission_test_runtime = Some(runtime);
    Ok(context)
}
