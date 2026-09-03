//! Per-project HTTP application router construction and mounting.
//!
//! Builds the dashboard/API router for one project and installs the cold
//! resolver the registry uses to mount it on demand.

use super::*;

#[hotpath::measure(label = "daemon.http.application.router_build")]
fn build_http_application_router(project_id: &str, project_path: &Path) -> Result<axum::Router> {
    let project_id = tracedecay_domain::ProjectId::new(project_id.to_owned()).map_err(|error| {
        TraceDecayError::Config {
            message: format!("daemon HTTP project identity is invalid: {error}"),
        }
    })?;
    let handshake = crate::daemon::handshake_for_current_client(
        Some(project_path.to_path_buf()),
        None,
        false,
        false,
    )?;
    let client = tracedecay_daemon_identity::invocation_client_for_current(handshake)?;
    crate::application_surface::http_application_router(
        client,
        tracedecay_daemon_service::daemon_operation_event_authority(),
        project_id.clone(),
    )
    .map_err(|error| TraceDecayError::Config {
        message: format!("could not mount daemon HTTP application routes: {error}"),
    })
}

pub(super) fn install_http_application_cold_resolver(
    registry: &http_application::DaemonHttpApplicationRegistry,
    store_administration: StoreAdministration,
    invocation: DaemonInvocationState,
    project_open_gates: Arc<tokio::sync::Mutex<ProjectOpenGates>>,
) -> Result<()> {
    registry.install_remote_deletion_runtime_owners(
        super::remote_deletion::RemoteDeletionRuntimeOwners {
            administration: store_administration.clone(),
            invocation,
            project_open_gates,
        },
    )?;
    registry.install_resolver(move |project_id| {
        let store_administration = store_administration.clone();
        hotpath::future!(
            async move {
                let database = store_administration.registered_profile_database().await?;
                let profile_id = store_administration
                    .profile_identity()?
                    .profile_id()
                    .as_str();
                if database
                    .remote_deletion_tombstone_for_project(profile_id, project_id.as_str())
                    .await?
                    .is_some()
                {
                    return Ok(None);
                }
                let Some(context) = database
                    .project_registry_context_by_id(project_id.as_str())
                    .await?
                else {
                    return Ok(None);
                };
                if context.project.project_id != project_id.as_str() {
                    return Err(TraceDecayError::Config {
                        message: "daemon HTTP project registry identity changed".to_owned(),
                    });
                }
                let registered_root = PathBuf::from(&context.project.canonical_root);
                if !registered_root.is_absolute() {
                    return Err(TraceDecayError::Config {
                        message: "daemon HTTP registered project root is not absolute".to_owned(),
                    });
                }
                let canonical_root =
                    registered_root
                        .canonicalize()
                        .map_err(|error| TraceDecayError::Config {
                            message: format!(
                                "daemon HTTP registered project root is unavailable: {error}"
                            ),
                        })?;
                if canonical_root != registered_root {
                    return Err(TraceDecayError::Config {
                        message: "daemon HTTP registered project root is not canonical".to_owned(),
                    });
                }
                build_http_application_router(project_id.as_str(), &canonical_root).map(Some)
            },
            label = "daemon.http.application.router_cold_resolve"
        )
    })
}

#[hotpath::measure(future = true, label = "daemon.http.application.remote_router_install")]
pub(super) async fn install_remote_http_application_router(
    registry: &http_application::DaemonHttpApplicationRegistry,
    store_administration: &StoreAdministration,
    invocation: &DaemonInvocationState,
) -> Result<()> {
    let runtime = store_administration.registered_runtime_registry().await?;
    let credentials = runtime.remote_credential_authority();
    let router = super::remote_protocol::build_daemon_remote_protocol_router(
        Arc::clone(&credentials),
        runtime.remote_replay_transaction(),
        invocation.service.clone(),
    )?;
    registry.install_remote(router, credentials, Some(runtime))
}

#[hotpath::measure(future = true, label = "daemon.http.application.router_mount")]
pub(super) async fn mount_http_application_router(
    registry: &http_application::DaemonHttpApplicationRegistry,
    project_id: &str,
    project_path: &Path,
) -> Result<()> {
    if !registry.is_active() {
        return Ok(());
    }
    let router = build_http_application_router(project_id, project_path)?;
    registry.mount(project_id, router).await
}
