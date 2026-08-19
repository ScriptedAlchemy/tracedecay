//! Project-open owner task for the feedback and advisory owners.
//!
//! One task per project open owns every mount of the configuration-pinned
//! feedback/advisory chain: the initial deferred mount behind the first sealed
//! code-index generation, and every configuration remount requested by the
//! hook-cycle producer when the pinned revision drifts (for example the Plan
//! 20 Context Scout flag toggled after the project opened).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::{broadcast, mpsc};
use tracedecay_application::now_micros;
use tracedecay_usecases::configuration::ConfigurationCurrentStateV1;

use super::super::{project_open_lsp_scope_grant, register_production_lsp_owner};
use super::{
    DaemonInvocationState, ProjectOpenDependentOwnerState,
    register_production_feedback_and_advisory,
};
use crate::daemon::code_index_scheduler::CodeIndexGenerationPublishedV1;

/// Whether the owner task starts with the advisory chain already mounted.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum MountPhase {
    Mounted,
    NeedsMount,
}

pub(super) fn spawn(
    invocation: DaemonInvocationState,
    project_root: PathBuf,
    mut state: ProjectOpenDependentOwnerState,
    phase: MountPhase,
    remount_tx: mpsc::Sender<()>,
    mut remount_rx: mpsc::Receiver<()>,
) {
    tokio::spawn(async move {
        let mut publications = invocation
            .code_index_schedulers
            .subscribe_generation_publications();
        if phase == MountPhase::NeedsMount
            && !mount_until_settled(
                &invocation,
                &project_root,
                &mut state,
                &remount_tx,
                &mut publications,
            )
            .await
        {
            return;
        }
        loop {
            tokio::select! {
                signal = remount_rx.recv() => {
                    if signal.is_none() {
                        return;
                    }
                }
                publication = publications.recv() => {
                    match publication {
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                        _ => continue,
                    }
                }
            }
            let Ok(pinned) = state.graph.configuration_runtime().client().current().await else {
                // The next drifting hook cycle re-signals; a transient
                // configuration read failure must not wedge the task.
                continue;
            };
            if pinned.revision_id == state.scout_configuration.revision_id {
                continue;
            }
            state.scout_configuration = ConfigurationCurrentStateV1 {
                revision_id: pinned.revision_id,
                snapshot: pinned.snapshot,
            };
            if invocation
                .advisory_runtime_registrar()
                .withdraw_for_reconfiguration(&project_root)
                .await
                .is_err()
            {
                return;
            }
            tracing::info!(
                event = "feedback_advisory_mount",
                outcome = "reconfiguring",
                project = %project_root.display(),
                revision = state.scout_configuration.revision_id.as_str(),
                "rebuilding the feedback and advisory owners under the current configuration"
            );
            if !mount_until_settled(
                &invocation,
                &project_root,
                &mut state,
                &remount_tx,
                &mut publications,
            )
            .await
            {
                return;
            }
        }
    });
}

/// Drives mount attempts until the chain settles (mounted or given up) or the
/// publication channel closes. Returns `false` only when the task must end.
async fn mount_until_settled(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    state: &mut ProjectOpenDependentOwnerState,
    remount_tx: &mpsc::Sender<()>,
    publications: &mut broadcast::Receiver<CodeIndexGenerationPublishedV1>,
) -> bool {
    let mut partial_publication_retried = false;
    loop {
        match try_mount(invocation, project_root, state, remount_tx).await {
            Attempt::Terminal => return true,
            Attempt::RetryPartialPublication if !partial_publication_retried => {
                partial_publication_retried = true;
                tokio::task::yield_now().await;
                continue;
            }
            Attempt::RetryPartialPublication => return true,
            Attempt::AwaitNextPublication => {}
        }
        break;
    }
    loop {
        match publications.recv().await {
            Ok(publication) if publication.project_root == project_root => {}
            Ok(_) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return false,
        }
        match try_mount(invocation, project_root, state, remount_tx).await {
            Attempt::Terminal => return true,
            Attempt::AwaitNextPublication => {}
            Attempt::RetryPartialPublication => {
                tokio::task::yield_now().await;
                if try_mount(invocation, project_root, state, remount_tx).await
                    != Attempt::AwaitNextPublication
                {
                    return true;
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Attempt {
    Terminal,
    AwaitNextPublication,
    RetryPartialPublication,
}

async fn try_mount(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    state: &mut ProjectOpenDependentOwnerState,
    remount_tx: &mpsc::Sender<()>,
) -> Attempt {
    if let Some(lsp_session_factory) = state.lsp_session_factory.as_ref() {
        return match register_production_feedback_and_advisory(
            invocation,
            project_root,
            state,
            Arc::clone(lsp_session_factory),
            remount_tx,
        )
        .await
        {
            Ok(()) => Attempt::Terminal,
            Err(_) => classify_failure(invocation, project_root, state).await,
        };
    }
    let Some(generation) = invocation
        .code_index_schedulers
        .latest_complete_ready_decoded_for_root_scope(project_root, &state.scope)
        .await
    else {
        return Attempt::AwaitNextPublication;
    };
    let mut indexed_files = generation
        .generation()
        .snapshot()
        .files
        .iter()
        .map(|file| file.logical_path.clone())
        .collect::<Vec<_>>();
    indexed_files.sort();
    let admitted_providers = {
        let mut broker = state.diagnostic_broker.lock().await;
        let admitted = broker.admitted_providers_for_files(&indexed_files);
        state.mounted_providers = broker.mounted_providers_for_files(&indexed_files);
        admitted
    };
    let scope_grant = match project_open_lsp_scope_grant(&state.access, now_micros()) {
        Ok(grant) => grant,
        Err(error) => {
            tracing::warn!(
                event = "feedback_advisory_mount",
                outcome = "deferred_failed",
                project = %project_root.display(),
                reason = %error,
                "deferred advisory LSP grant is unavailable"
            );
            return Attempt::Terminal;
        }
    };
    let lsp_session_factory = match register_production_lsp_owner(
        invocation,
        project_root,
        scope_grant,
        state.session_db.clone(),
        state.database.clone(),
        Arc::clone(&state.diagnostic_broker),
        &admitted_providers,
        state.admitted_root_uri.clone(),
    )
    .await
    {
        Ok(factory) => factory,
        Err(error) => {
            tracing::warn!(
                event = "feedback_advisory_mount",
                outcome = "deferred_failed",
                project = %project_root.display(),
                reason = %error,
                "deferred advisory LSP owner could not mount"
            );
            return Attempt::Terminal;
        }
    };
    state.lsp_session_factory = Some(Arc::clone(&lsp_session_factory));
    match register_production_feedback_and_advisory(
        invocation,
        project_root,
        state,
        lsp_session_factory,
        remount_tx,
    )
    .await
    {
        Ok(()) => {
            tracing::info!(
                event = "feedback_advisory_mount",
                outcome = "mounted",
                project = %project_root.display(),
                deferred = true,
            );
        }
        Err(error) => {
            tracing::warn!(
                event = "feedback_advisory_mount",
                outcome = "deferred_failed",
                project = %project_root.display(),
                reason = %error,
                "deferred advisory owner could not mount"
            );
            return classify_failure(invocation, project_root, state).await;
        }
    }
    Attempt::Terminal
}

async fn classify_failure(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    state: &ProjectOpenDependentOwnerState,
) -> Attempt {
    if invocation
        .service
        .feedback_cycle(Some(project_root))
        .await
        .is_some()
    {
        Attempt::RetryPartialPublication
    } else if invocation
        .code_index_schedulers
        .latest_complete_ready_for_scope(&state.scope)
        .await
        .is_none()
    {
        Attempt::AwaitNextPublication
    } else {
        Attempt::Terminal
    }
}
