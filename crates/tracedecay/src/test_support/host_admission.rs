//! Root MCP composition over the registered host-admission test runtime.
//!
//! The runtime itself, registered databases, session registry, and the
//! project-graph opens through it, lives in `tracedecay-project`; this
//! module adds the direct MCP server construction context that needs the
//! root's MCP server.

use std::sync::Arc;

use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_sessions::admission::HostAdmissionScope;

use tracedecay_project::project::TraceDecay;
use tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1;

/// The owner a test runtime serves: its profile, with the profile's parent as
/// the home its host transcripts live under.
fn test_runtime_profile(
    runtime: &HostAdmissionTestRuntimeV1,
) -> tracedecay_runtime_core::config::ProfileRoot {
    let profile_root = runtime.profile_root_for_test();
    let profile = tracedecay_runtime_core::config::ProfileRoot::new(profile_root);
    match profile_root.parent() {
        Some(home) => profile.with_home(home),
        None => profile,
    }
}

/// A direct MCP server construction context bound to this runtime's
/// registered databases, profile identity, and background CPU authority.
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
    context.profile = Some(test_runtime_profile(&runtime));
    context.profile_identity = Some(Arc::new(profile_identity));
    context.background_cpu = Some(runtime.background_cpu());
    context.host_admission_test_runtime = Some(runtime);
    Ok(context)
}
