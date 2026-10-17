//! Per-project HTTP application router construction and mounting.
//!
//! Builds the dashboard/API router for one project and installs the cold
//! resolver the registry uses to mount it on demand.

use super::*;
use tracedecay_runtime_core::logging::log_daemon_event;
use tracedecay_runtime_core::native_runtime;
use tracedecay_runtime_core::path_safety::canonical_existing_identity;

#[tracing::instrument(
    name = "daemon.http.application.router_build",
    level = "trace",
    skip_all
)]
async fn build_http_application_router(
    owner: &tracedecay_runtime_core::config::ProfileRoot,
    project_id: &str,
    project_path: &Path,
) -> Result<axum::Router> {
    let project_id = tracedecay_domain::ProjectId::new(project_id.to_owned()).map_err(|error| {
        TraceDecayError::Config {
            message: format!("daemon HTTP project identity is invalid: {error}"),
        }
    })?;
    let owner = owner.clone();
    let project_path = project_path.to_path_buf();
    // The build costs 0.1-1.5 s of CPU, so it stays off the caller's poll.
    native_runtime::spawn_blocking(move || {
        #[cfg(test)]
        tests::spend_injected_build_cpu(project_id.as_str());
        let handshake = crate::daemon::handshake_for_current_client(
            &owner,
            Some(project_path),
            None,
            false,
            false,
        )?;
        let client = crate::daemon::invocation_client_for_current(&owner, handshake)?;
        tracedecay_daemon_service::application_surface::http_application_router(
            client,
            tracedecay_daemon_service::daemon_operation_event_authority(),
            project_id,
        )
        .map_err(|error| TraceDecayError::Config {
            message: format!("could not mount daemon HTTP application routes: {error}"),
        })
    })
    .await
    .map_err(|error| TraceDecayError::Config {
        message: format!("daemon HTTP application router build did not finish: {error}"),
    })?
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
                    store_administration.owner_profile()?,
                    project_id.as_str(),
                    &canonical_root,
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
    let router = build_http_application_router(owner, project_id, project_path).await?;
    registry.mount(project_id, router).await
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Mutex, PoisonError};
    use std::task::Poll;
    use std::time::{Duration, Instant};

    use futures_util::future::{Either, select};
    use tracedecay_runtime_core::native_runtime;

    /// CPU a router build spends before its real work, by project, standing
    /// in for a build that runs long on a loaded host.
    static INJECTED_BUILD_CPU: Mutex<BTreeMap<String, Duration>> = Mutex::new(BTreeMap::new());

    pub(super) fn spend_injected_build_cpu(project_id: &str) {
        let cpu = INJECTED_BUILD_CPU
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

    /// The router build is synchronous and measured 0.1-1.5 s of CPU under
    /// load. Run inside the caller's native poll, it holds that worker and
    /// every future polled beside it until it returns (#4056).
    #[test]
    fn a_router_build_longer_than_one_native_poll_leaves_the_poll() {
        tracedecay_project::product_runtime::register_fixture_product_runtime();
        let project = tempfile::tempdir().expect("project root");
        let owner =
            tracedecay_runtime_core::config::ProfileRoot::new(project.path().join("profile"));
        let project_id = "http-router-build-off-native-poll";
        INJECTED_BUILD_CPU
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(project_id.to_owned(), native_runtime::LONG_POLL * 4);
        let peer_ran_first = native_runtime::block_on(async {
            let build = std::pin::pin!(super::build_http_application_router(
                &owner,
                project_id,
                project.path(),
            ));
            let mut yielded = false;
            let peer = std::future::poll_fn(|task| {
                if yielded {
                    return Poll::Ready(());
                }
                yielded = true;
                task.waker().wake_by_ref();
                Poll::Pending
            });
            match select(build, peer).await {
                Either::Left(_) => false,
                Either::Right(((), build)) => {
                    let _ = build.await;
                    true
                }
            }
        })
        .expect("native runtime runs the build");
        assert!(
            peer_ran_first,
            "a future polled beside the router build must progress while it runs"
        );
    }
}
