use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::super::{project_open_lsp_scope_grant, register_production_lsp_owner};
use super::{
    DaemonInvocationState, ProjectOpenDependentOwnerState,
    register_production_feedback_and_advisory,
};
use tracedecay_application::now_micros;

pub(super) fn spawn(
    invocation: DaemonInvocationState,
    project_root: PathBuf,
    mut state: ProjectOpenDependentOwnerState,
) {
    tokio::spawn(async move {
        let mut publications = invocation
            .code_index_schedulers
            .subscribe_generation_publications();
        let mut partial_publication_retried = false;
        loop {
            match try_mount(&invocation, &project_root, &mut state).await {
                Attempt::Terminal => return,
                Attempt::RetryPartialPublication if !partial_publication_retried => {
                    partial_publication_retried = true;
                    tokio::task::yield_now().await;
                    continue;
                }
                Attempt::RetryPartialPublication => return,
                Attempt::AwaitNextPublication => {}
            }
            break;
        }
        loop {
            match publications.recv().await {
                Ok(publication) if publication.project_root == project_root => {}
                Ok(_) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
            match try_mount(&invocation, &project_root, &mut state).await {
                Attempt::Terminal => return,
                Attempt::AwaitNextPublication => {}
                Attempt::RetryPartialPublication => {
                    tokio::task::yield_now().await;
                    if try_mount(&invocation, &project_root, &mut state).await
                        != Attempt::AwaitNextPublication
                    {
                        return;
                    }
                }
            }
        }
    });
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
) -> Attempt {
    if let Some(lsp_session_factory) = state.lsp_session_factory.as_ref() {
        return match register_production_feedback_and_advisory(
            invocation,
            project_root,
            state,
            Arc::clone(lsp_session_factory),
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
        Arc::clone(&state.session_db),
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
