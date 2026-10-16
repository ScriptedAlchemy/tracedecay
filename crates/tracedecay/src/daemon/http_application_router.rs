//! Per-project HTTP application router construction and mounting.
//!
//! Builds the dashboard/API router for one project and installs the cold
//! resolver the registry uses to mount it on demand.

use super::*;
use tracedecay_runtime_core::logging::log_daemon_event;
use tracedecay_runtime_core::path_safety::canonical_existing_identity;

/// The router build is synchronous CPU work, about 100 ms in a loaded debug
/// build. Inside a native task's poll it holds that worker's reactor, and
/// past `LONG_POLL_CPU` a debug build fails the task (#4066).
async fn build_http_application_router(
    owner: tracedecay_runtime_core::config::ProfileRoot,
    project_id: String,
    project_path: PathBuf,
) -> Result<axum::Router> {
    let span = tracing::trace_span!("daemon.http.application.router_build");
    tracedecay_runtime_core::native_runtime::spawn_blocking(move || {
        let _span = span.entered();
        build_http_application_router_now(&owner, &project_id, &project_path)
    })
    .await
    .map_err(|error| TraceDecayError::Config {
        message: format!("daemon HTTP application router build did not run: {error}"),
    })?
}

fn build_http_application_router_now(
    owner: &tracedecay_runtime_core::config::ProfileRoot,
    project_id: &str,
    project_path: &Path,
) -> Result<axum::Router> {
    #[cfg(test)]
    tests::spin_injected_router_build_cpu(project_id);
    let project_id = tracedecay_domain::ProjectId::new(project_id.to_owned()).map_err(|error| {
        TraceDecayError::Config {
            message: format!("daemon HTTP project identity is invalid: {error}"),
        }
    })?;
    let handshake = crate::daemon::handshake_for_current_client(
        owner,
        Some(project_path.to_path_buf()),
        None,
        false,
        false,
    )?;
    let client = crate::daemon::invocation_client_for_current(owner, handshake)?;
    tracedecay_daemon_service::application_surface::http_application_router(
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
        tracing::Instrument::instrument(
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
                    canonical_existing_identity(&registered_root).map_err(|error| {
                        TraceDecayError::Config {
                            message: format!(
                                "daemon HTTP registered project root is unavailable: {error}"
                            ),
                        }
                    })?;
                if canonical_root != registered_root {
                    return Err(TraceDecayError::Config {
                        message: "daemon HTTP registered project root is not canonical".to_owned(),
                    });
                }
                build_http_application_router(
                    store_administration.owner_profile()?.clone(),
                    project_id.as_str().to_owned(),
                    canonical_root,
                )
                .await
                .map(Some)
            },
            tracing::trace_span!("daemon.http.application.router_cold_resolve"),
        )
    })
}

#[tracing::instrument(
    name = "daemon.http.application.remote_router_install",
    level = "trace",
    skip_all
)]
pub(super) async fn install_remote_http_application_router(
    registry: &http_application::DaemonHttpApplicationRegistry,
    store_administration: &StoreAdministration,
    invocation: &DaemonInvocationState,
) -> Result<()> {
    let runtime = match store_administration.registered_runtime_registry().await {
        Ok(runtime) => runtime,
        // The account-active guard reads the profile authority. While that
        // store is reset-required no remote request can be admitted, so the
        // remote protocol stays unmounted until the operator's reset restarts
        // the daemon on a fresh profile.
        Err(error) if error.is_store_reset_required() => {
            log_daemon_event(
                "remote_protocol_router_unmounted",
                &[
                    ("reason", "profile_authority_reset_required".to_owned()),
                    ("error", error.to_string()),
                ],
            );
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let credentials = runtime.remote_credential_authority();
    let router = tracedecay_daemon_service::build_daemon_remote_protocol_router(
        Arc::clone(&credentials),
        runtime.remote_replay_transaction(),
        invocation.service.clone(),
    )?;
    registry.install_remote(router, credentials, Some(runtime))
}

#[tracing::instrument(
    name = "daemon.http.application.router_mount",
    level = "trace",
    skip_all
)]
pub(super) async fn mount_http_application_router(
    registry: &http_application::DaemonHttpApplicationRegistry,
    owner: &tracedecay_runtime_core::config::ProfileRoot,
    project_id: &str,
    project_path: &Path,
) -> Result<()> {
    if !registry.is_active() {
        return Ok(());
    }
    let router = build_http_application_router(
        owner.clone(),
        project_id.to_owned(),
        project_path.to_path_buf(),
    )
    .await?;
    registry.mount(project_id, router).await
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Mutex, PoisonError};
    use std::time::{Duration, Instant};

    static INJECTED_ROUTER_BUILD_CPU: Mutex<BTreeMap<String, Duration>> =
        Mutex::new(BTreeMap::new());

    pub(super) fn spin_injected_router_build_cpu(project_id: &str) {
        let cpu = INJECTED_ROUTER_BUILD_CPU
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(project_id);
        if let Some(cpu) = cpu {
            let spun = Instant::now();
            while spun.elapsed() < cpu {
                std::hint::spin_loop();
            }
        }
    }

    #[cfg(all(unix, debug_assertions))]
    #[test]
    fn a_router_build_longer_than_one_native_poll_does_not_fail_its_task() {
        use tracedecay_runtime_core::native_runtime::{self, NativeTask, NativeTaskOutcome};

        const PROJECT_ID: &str = "project.long-router-build";
        INJECTED_ROUTER_BUILD_CPU
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(PROJECT_ID.to_owned(), native_runtime::LONG_POLL_CPU * 2);
        let profile = tempfile::tempdir().expect("profile root");
        let owner = tracedecay_runtime_core::config::ProfileRoot::new(profile.path().to_path_buf());
        let project_path = profile.path().to_path_buf();
        // The build may refuse this bare profile; only where it ran is under test.
        let mount = NativeTask::spawn(async move {
            let _ =
                super::build_http_application_router(owner, PROJECT_ID.to_owned(), project_path)
                    .await;
        })
        .expect("native mount task");
        assert_eq!(
            native_runtime::block_on(mount.wait()).expect("process runtime"),
            NativeTaskOutcome::Completed(()),
        );
    }
}
