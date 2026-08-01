//! Per-project HTTP application router construction and mounting.
//!
//! Builds the dashboard/API router for one project and installs the cold
//! resolver the registry uses to mount it on demand.
//!
//! Relocated verbatim from `daemon.rs` as a pure structural split; no logic
//! or signatures changed. `use super::*` re-exposes every name the parent
//! `daemon` module had in scope so the moved code resolves unchanged.

use super::*;

fn build_http_application_router(project_id: &str, project_path: &Path) -> Result<axum::Router> {
    let project_id = tracedecay_domain::ProjectId::new(project_id.to_owned()).map_err(|error| {
        TraceDecayError::Config {
            message: format!("daemon HTTP project identity is invalid: {error}"),
        }
    })?;
    let handshake =
        DaemonHandshake::for_current_client(Some(project_path.to_path_buf()), None, false, false)?;
    let client = crate::daemon_client::DaemonInvocationClient::for_current(handshake)?;
    crate::application_surface::http_application_router(
        client,
        daemon_operation_event_authority(),
        project_id.clone(),
    )
    .map_err(|error| TraceDecayError::Config {
        message: format!("could not mount daemon HTTP application routes: {error}"),
    })
}

pub(super) fn install_http_application_cold_resolver(
    registry: &http_application::DaemonHttpApplicationRegistry,
    store_administration: StoreAdministration,
) -> Result<()> {
    registry.install_resolver(move |project_id| {
        let store_administration = store_administration.clone();
        async move {
            let database = store_administration.registered_profile_database().await?;
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
        }
    })
}

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
