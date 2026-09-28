//! Opens registered projects whose hook spools hold events no running
//! replay consumer will drain: at startup for every project with spooled
//! records, and afterwards for each append the spool watch reports on a
//! project that is not open. Only the Unix daemon composes a `DaemonEngine`.

use std::path::{Path, PathBuf};

use tokio::sync::mpsc;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_hooks::{HookSpoolV1, hook_v2_spool_root};
use tracedecay_runtime_core::config::ProfileRoot;

use super::spool_watch::{SpooledProject, consumer_attached, install_opener, watch_project};
use crate::daemon::engine::DaemonEngine;
use crate::daemon::project_open_admission::ProjectOpenTaskClaim;
use crate::daemon::project_routing::{
    bind_authenticated_profile_identity, project_open_task_capacity_error,
};

const REGISTRY_PAGE: usize = 256;

fn has_spooled_records(data_root: &Path) -> bool {
    tracedecay_agent_hosts::hooks::NATIVE_HOOK_HOSTS
        .iter()
        .any(|host| {
            HookSpoolV1::has_records(&hook_v2_spool_root(data_root, *host)).unwrap_or(false)
        })
}

/// Starts the daemon's spooled-hook opener: watches every registered project,
/// opens the ones already holding spooled records, then opens each project
/// whose spool receives an append while it is not open. Runs until the
/// daemon drains.
pub(in crate::daemon) fn spawn_spooled_hook_opener(
    engine: DaemonEngine,
    profile: ProfileRoot,
) -> tokio::task::JoinHandle<()> {
    let (opener, mut requests) = mpsc::unbounded_channel();
    install_opener(opener.clone());
    tokio::spawn(async move {
        let draining = engine.lifecycle.clone();
        let work = async {
            match registered_projects(&engine, &profile).await {
                Ok(projects) => {
                    for project in projects {
                        watch_project(&project, None);
                        if has_spooled_records(&project.data_root) {
                            let _ = opener.send(project);
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
                if let Err(error) = open_for_spooled_hooks(&engine, &profile, &project).await {
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
            () = draining.wait_for_draining() => {}
        }
    })
}

async fn registered_projects(
    engine: &DaemonEngine,
    profile: &ProfileRoot,
) -> Result<Vec<SpooledProject>> {
    let registry = engine
        .store_administration
        .registered_profile_database()
        .await?;
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
    engine: &DaemonEngine,
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
    let administration =
        bind_authenticated_profile_identity(&mut handshake, &engine.store_administration).await?;
    let mut engine = engine.clone();
    engine.store_administration = administration;
    match engine.begin_project_open(handshake, None).await? {
        ProjectOpenTaskClaim::InFlight(_) => Ok(()),
        ProjectOpenTaskClaim::Failed(failure) => Err(failure.to_error()),
        ProjectOpenTaskClaim::Saturated => Err(project_open_task_capacity_error()),
    }
}
