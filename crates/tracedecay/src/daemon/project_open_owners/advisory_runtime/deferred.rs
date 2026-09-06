use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::super::{project_open_lsp_scope_grant, register_production_lsp_owner};
use super::{
    DaemonInvocationState, ProjectOpenDependentOwnerState, register_production_advisory_owner,
    register_production_feedback_and_advisory, register_production_feedback_cycle,
};
use crate::daemon::log_daemon_event;
use tracedecay_application::now_micros;

/// The deferred advisory owner is a detached background task: when it gives up
/// (or never sees a publication) nothing in the request path reports it, and a
/// project silently serves without a feedback cycle. Record every attempt
/// outcome on the daemon event stream so that state is diagnosable.
fn log_deferred_attempt(project_root: &Path, phase: &str, attempt: &str) {
    log_daemon_event(
        "advisory_deferred_attempt",
        &[
            ("project", project_root.display().to_string()),
            ("phase", phase.to_owned()),
            ("attempt", attempt.to_owned()),
        ],
    );
}

pub(super) fn spawn(
    owner: &crate::mcp::McpServer,
    invocation: DaemonInvocationState,
    project_root: PathBuf,
    mut state: ProjectOpenDependentOwnerState,
) -> bool {
    owner.spawn_background_task(hotpath::future!(
        async move {
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
        },
        label = "daemon.project.owners.advisory_deferred"
    ))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Attempt {
    Terminal,
    AwaitNextPublication,
    RetryPartialPublication,
}

#[hotpath::measure(label = "daemon.project.owners.advisory_retry", future = true)]
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
    // Two lookups, in this order, because passive waiting alone deadlocks a
    // fresh project. The decoded-for-root-scope probe is the cheap arm: it
    // reads an already-seated complete generation and asks the scheduler for
    // nothing. When nothing is seated it answers `None` and demands nothing,
    // so a deferred owner that only ever took this arm waited for a
    // publication that only demand produces — the project then served
    // indefinitely with the typed-unavailable feedback cycle.
    // `latest_complete_ready_for_scope` is the authenticated demand boundary
    // every other first-generation consumer resolves through, so take it
    // before giving up and going back to sleep.
    let indexed = match invocation
        .code_index_schedulers
        .latest_complete_ready_decoded_for_root_scope(project_root, &state.scope)
        .await
    {
        Some(generation) => Some(generation.text_generation_handle()),
        None => match invocation
            .code_index_schedulers
            .latest_complete_ready_for_scope(&state.scope)
            .await
        {
            Some(generation) => Some(generation.text_generation_handle()),
            // A clean restart that recovered its retained revision-7 graph
            // head serves through the text projection and never seats the
            // sealed slot, so no publication edge follows for a quiet
            // checkout. Feedback, session and LSP availability must not wait
            // on full code-index publication: take that recovered level,
            // which carries the same sealed snapshot this owner reads.
            None => invocation
                .code_index_schedulers
                .latest_text_serving_for_scope(&state.scope)
                .await,
        },
    };
    let Some(indexed) = indexed else {
        log_deferred_attempt(
            project_root,
            "generation_unavailable",
            "await_next_publication",
        );
        return Attempt::AwaitNextPublication;
    };
    let mut indexed_files = indexed
        .metadata()
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
    // Feedback first, then the LSP owner it feeds. The published feedback
    // cycle is what every reader (and `DaemonInvocationService::feedback_cycle`)
    // treats as "this project's diagnostics authority"; upgrading the LSP owner
    // ahead of it publishes a provider-backed gateway for a project whose
    // feedback cycle is still the typed-unavailable placeholder, so a
    // diagnostics publication that lands in that window has nowhere truthful to
    // go. The cycle depends only on the sealed generation this attempt already
    // holds, not on the session factory — only the advisory owner needs that.
    let (feedback_cycle, feedback_scope) =
        match register_production_feedback_cycle(invocation, project_root, state).await {
            Ok(mounted) => mounted,
            Err(error) => {
                tracing::warn!(
                    event = "feedback_advisory_mount",
                    outcome = "deferred_failed",
                    project = %project_root.display(),
                    reason = %error,
                    "deferred feedback cycle could not mount"
                );
                log_deferred_attempt(project_root, "feedback_cycle_failed", &error.to_string());
                return classify_failure(invocation, project_root, state).await;
            }
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
    match register_production_advisory_owner(
        invocation,
        project_root,
        state,
        feedback_cycle,
        feedback_scope,
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
            log_deferred_attempt(project_root, "mounted", "terminal");
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
        && invocation
            .code_index_schedulers
            .latest_text_serving_for_scope(&state.scope)
            .await
            .is_none()
    {
        Attempt::AwaitNextPublication
    } else {
        Attempt::Terminal
    }
}
