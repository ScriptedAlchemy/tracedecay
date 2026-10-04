//! Opens registered projects whose hook spools hold events or delivery
//! receipts no running replay consumer will drain: at startup for every
//! project with spooled records or receipts, and afterwards for each append
//! or receipt the spool watch reports on a project that is not open. The Unix
//! daemon opens through its `DaemonEngine`; the portable daemon through the
//! project-server warmup a client request would take.

use std::path::{Path, PathBuf};
#[cfg(not(unix))]
use std::sync::Arc;

use tokio::sync::mpsc;
use tracedecay_daemon_service::shutdown::DaemonLifecycle;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_hooks::{
    HookDeliveryReceiptSpoolV1, HookSpoolV1, hook_delivery_receipt_spool_root, hook_v2_spool_root,
};
use tracedecay_runtime_core::config::ProfileRoot;

use super::spool_watch::{SpooledProject, consumer_attached, install_opener, watch_project};
#[cfg(not(unix))]
use crate::daemon::DaemonInvocationState;
use crate::daemon::branch_admin::StoreAdministration;
#[cfg(unix)]
use crate::daemon::engine::DaemonEngine;
#[cfg(not(unix))]
use crate::daemon::project_open_admission::ProjectOpenGates;
#[cfg(unix)]
use crate::daemon::project_open_admission::ProjectOpenTaskClaim;
#[cfg(not(unix))]
use crate::daemon::project_open_orchestration::schedule_portable_project_server_warmup;
use crate::daemon::project_routing::bind_authenticated_profile_identity;
#[cfg(unix)]
use crate::daemon::project_routing::project_open_task_capacity_error;

const REGISTRY_PAGE: usize = 256;

/// The owners a portable daemon needs to open a project for its spool: the
/// same bundle a client connection carries into
/// [`schedule_portable_project_server_warmup`], plus the lifecycle the opener
/// drains with.
#[cfg(not(unix))]
pub(in crate::daemon) struct PortableSpoolOpenerOwners {
    pub lifecycle: DaemonLifecycle,
    pub store_administration: StoreAdministration,
    pub project_open_gates: Arc<tokio::sync::Mutex<ProjectOpenGates>>,
    pub invocation: DaemonInvocationState,
    pub http_application_registry: crate::daemon::http_application::DaemonHttpApplicationRegistry,
}

/// How `open_for_spooled_hooks` mounts a project on this platform.
enum SpoolOpener {
    #[cfg(unix)]
    Engine(DaemonEngine),
    #[cfg(not(unix))]
    Portable(PortableSpoolOpenerOwners),
}

/// Whether any host spool holds records or receipts to drain. A spool that
/// cannot be read is reported and counted as holding work, so the project's
/// replay consumer surfaces the fault instead of the opener hiding it.
fn has_spooled_records(data_root: &Path) -> bool {
    tracedecay_agent_hosts::hooks::NATIVE_HOOK_HOSTS
        .iter()
        .any(|host| {
            let records = HookSpoolV1::has_records(&hook_v2_spool_root(data_root, *host))
                .map_err(|error| error.to_string());
            let receipts = HookDeliveryReceiptSpoolV1::has_receipts(
                &hook_delivery_receipt_spool_root(data_root, *host),
            )
            .map_err(|error| error.to_string());
            match (records, receipts) {
                (Ok(records), Ok(receipts)) => records || receipts,
                (Err(error), _) | (_, Err(error)) => {
                    tracing::warn!(
                        host = host.hook_key(),
                        %error,
                        data_root = %data_root.display(),
                        "hook spool could not be inspected at startup; opening its project to drain it"
                    );
                    true
                }
            }
        })
}

/// Starts the daemon's spooled-hook opener on Unix: watches every registered
/// project, opens the ones already holding spooled records, then opens each
/// project whose spool receives an append while it is not open. Runs until
/// the daemon drains.
#[cfg(unix)]
pub(in crate::daemon) fn spawn_spooled_hook_opener(
    engine: DaemonEngine,
    profile: ProfileRoot,
) -> tokio::task::JoinHandle<()> {
    spawn_spool_opener(
        engine.store_administration.clone(),
        engine.lifecycle.clone(),
        SpoolOpener::Engine(engine),
        profile,
    )
}

/// Starts the daemon's spooled-hook opener on the portable daemon. Identical
/// to the Unix entry point except project opens go through the portable
/// project-server warmup instead of the engine.
#[cfg(not(unix))]
pub(in crate::daemon) fn spawn_spooled_hook_opener(
    owners: PortableSpoolOpenerOwners,
    profile: ProfileRoot,
) -> tokio::task::JoinHandle<()> {
    spawn_spool_opener(
        owners.store_administration.clone(),
        owners.lifecycle.clone(),
        SpoolOpener::Portable(owners),
        profile,
    )
}

fn spawn_spool_opener(
    store_administration: StoreAdministration,
    lifecycle: DaemonLifecycle,
    opener: SpoolOpener,
    profile: ProfileRoot,
) -> tokio::task::JoinHandle<()> {
    let (wakes, mut requests) = mpsc::unbounded_channel();
    install_opener(wakes.clone());
    tokio::spawn(async move {
        let work = async {
            match registered_projects(&store_administration, &profile).await {
                Ok(projects) => {
                    for project in projects {
                        watch_project(&project, None);
                        if has_spooled_records(&project.data_root) {
                            let _ = wakes.send(project);
                        }
                    }
                }
                Err(error) => tracing::warn!(
                    %error,
                    "registered projects could not be read; spooled hook events wait until each project opens"
                ),
            }
            while let Some(project) = requests.recv().await {
                if consumer_attached(&project.data_root) {
                    continue;
                }
                if let Err(error) = open_for_spooled_hooks(&opener, &profile, &project).await {
                    tracing::warn!(
                        %error,
                        project = %project.project_root.display(),
                        "project holding spooled hook events could not be opened"
                    );
                }
            }
        };
        tokio::select! {
            () = work => {}
            () = lifecycle.wait_for_draining() => {}
        }
    })
}

async fn registered_projects(
    store_administration: &StoreAdministration,
    profile: &ProfileRoot,
) -> Result<Vec<SpooledProject>> {
    let registry = store_administration.registered_profile_database().await?;
    let mut roots = Vec::new();
    let mut after = None::<String>;
    loop {
        let page = registry
            .list_code_projects_after(after.as_deref(), REGISTRY_PAGE)
            .await?;
        let Some(last) = page.last() else {
            break;
        };
        after = Some(last.project_id.clone());
        let full_page = page.len() == REGISTRY_PAGE;
        roots.extend(
            page.into_iter()
                .map(|record| PathBuf::from(record.canonical_root)),
        );
        if !full_page {
            break;
        }
    }
    let profile_root = profile.data_dir().to_path_buf();
    tokio::task::spawn_blocking(move || {
        roots
            .into_iter()
            .filter_map(|project_root| {
                match tracedecay_runtime_core::storage::resolve_persisted_layout(
                    &project_root,
                    &profile_root,
                ) {
                    Ok(Some(layout)) => Some(SpooledProject {
                        project_root,
                        data_root: layout.data_root,
                    }),
                    Ok(None) => None,
                    Err(error) => {
                        tracing::warn!(
                            %error,
                            project = %project_root.display(),
                            "registered project layout could not be resolved for its hook spool"
                        );
                        None
                    }
                }
            })
            .collect()
    })
    .await
    .map_err(|error| TraceDecayError::Config {
        message: format!("registered project layout resolution failed: {error}"),
    })
}

/// Opens `project` with the daemon's own profile identity, exactly as a
/// client request for it would. Project composition registers the replay
/// consumer, whose first pass drains the spool.
async fn open_for_spooled_hooks(
    opener: &SpoolOpener,
    profile: &ProfileRoot,
    project: &SpooledProject,
) -> Result<()> {
    let mut handshake = crate::daemon::handshake_for_current_client(
        profile,
        Some(project.project_root.clone()),
        None,
        false,
        false,
    )?;
    match opener {
        #[cfg(unix)]
        SpoolOpener::Engine(engine) => {
            let administration =
                bind_authenticated_profile_identity(&mut handshake, &engine.store_administration)
                    .await?;
            let mut engine = engine.clone();
            engine.store_administration = administration;
            match engine.begin_project_open(handshake, None).await? {
                ProjectOpenTaskClaim::InFlight(_) => Ok(()),
                ProjectOpenTaskClaim::Failed(failure) => Err(failure.to_error()),
                ProjectOpenTaskClaim::Saturated => Err(project_open_task_capacity_error()),
            }
        }
        #[cfg(not(unix))]
        SpoolOpener::Portable(owners) => {
            let administration =
                bind_authenticated_profile_identity(&mut handshake, &owners.store_administration)
                    .await?;
            schedule_portable_project_server_warmup(
                owners.lifecycle.clone(),
                administration,
                Arc::clone(&owners.project_open_gates),
                owners.invocation.clone(),
                owners.http_application_registry.clone(),
                handshake,
                None,
                #[cfg(test)]
                None,
            )
            .await
        }
    }
}
