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

#[cfg(test)]
fn git(root: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new(
        tracedecay_runtime_core::git::try_git_program()
            .expect("absolute git executable should resolve"),
    )
    .current_dir(root)
    .args(args)
    .status()
    .expect("git command should run");
    assert!(status.success(), "git {args:?} failed");
}

/// [`mcp_server_context_for_test`] over a one-commit git project registered
/// as `project_id` in an isolated profile. Returns the project directory,
/// then the profile directory, beside the context.
#[cfg(test)]
pub(crate) async fn registered_git_project_context_for_test(
    project_id: &str,
) -> (
    crate::mcp::server::McpServerConstructionContext,
    tempfile::TempDir,
    tempfile::TempDir,
) {
    let profile = tempfile::TempDir::new().expect("isolated profile");
    let dir = tempfile::TempDir::new().expect("temp project");
    git(dir.path(), &["init", "-q", "-b", "main"]);
    git(dir.path(), &["config", "user.email", "test@example.com"]);
    git(dir.path(), &["config", "user.name", "Test"]);
    std::fs::write(dir.path().join(".gitignore"), ".tracedecay/\n").expect("gitignore");
    std::fs::create_dir_all(dir.path().join("src")).expect("source directory");
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "pub fn value() -> u8 { 1 }\n",
    )
    .expect("source");
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "initial"]);
    let runtime = HostAdmissionTestRuntimeV1::project(
        profile.path(),
        dir.path(),
        tracedecay_domain::ProjectId::new(project_id).expect("typed project identity"),
    )
    .await
    .expect("registered runtime");
    let graph = runtime
        .initialize_project_graph_for_test(
            dir.path(),
            tracedecay_project::project::TraceDecayOpenOptions {
                profile_root: Some(profile.path().to_path_buf()),
                global_db_path: None,
            },
        )
        .await
        .expect("daemon-owned project init");
    let context = mcp_server_context_for_test(Arc::new(runtime), graph, None)
        .expect("registered MCP server context");
    (context, dir, profile)
}
