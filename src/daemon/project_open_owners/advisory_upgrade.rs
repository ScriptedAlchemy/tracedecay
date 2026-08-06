//! Background upgrades from a served project to a feedback/advisory project.
//!
//! P3: the advisory owner (Context Scout config install, GitHub provider
//! resolution — potentially network — CI stores, and code-index-coupled
//! anchors) is NOT required for the project to be servable: when the feedback
//! cycle is skipped, advisory is skipped entirely and the project still fully
//! publishes and answers queries. Awaiting it on the open critical path
//! coupled open to the starved reconcile lane and to network stalls (observed
//! 924 s). Both upgrades therefore run as background tasks after open
//! returns; the advisory registrars are idempotent (AlreadyRegistered) and
//! self-contained.
//!
//! The deferred feedback upgrade covers the cold-open gap: the dependent
//! owner phase can run before the first code-index generation seals, in which
//! case the provider identity is transiently unresolvable. Rather than
//! disabling the feedback and advisory journey for the whole daemon session,
//! the registration retries when the scheduler publishes a generation for the
//! project.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracedecay_domain::feedback::FeedbackScopeV1;

use super::{
    DaemonInvocationState, ProductionFeedbackCycleRegistrationV1, ProjectOpenDependentOwnerState,
    register_production_advisory_owner, register_production_feedback_cycle,
};
use crate::daemon::code_index_scheduler::CodeIndexGenerationPublishedV1;

/// Registers the advisory owner for an already registered feedback cycle as a
/// background upgrade so project open returns as soon as the graph mounts.
pub(super) fn spawn_advisory_owner_upgrade(
    invocation: DaemonInvocationState,
    project_root: PathBuf,
    state: Arc<ProjectOpenDependentOwnerState>,
    feedback_cycle: Arc<crate::application::feedback::FeedbackCycleRuntime>,
    feedback_scope: FeedbackScopeV1,
    feedback_lsp_input: crate::application::feedback::FeedbackCycleLspInput,
) {
    tokio::spawn(async move {
        register_advisory_owner_from_state(
            &invocation,
            &project_root,
            &state,
            feedback_cycle,
            feedback_scope,
            feedback_lsp_input,
        )
        .await;
    });
}

/// Waits for the first complete code-index generation of `project_root` and
/// then retries feedback-cycle registration, upgrading to the advisory owner
/// on success. Exits when the publication channel closes (daemon shutdown) or
/// when the retry resolves to a session-permanent skip.
pub(super) fn spawn_deferred_feedback_cycle_upgrade(
    invocation: DaemonInvocationState,
    project_root: PathBuf,
    state: Arc<ProjectOpenDependentOwnerState>,
    mut generation_publications: tokio::sync::broadcast::Receiver<CodeIndexGenerationPublishedV1>,
) {
    tokio::spawn(async move {
        // Generations build on demand, so a passive wait could outlive the
        // session on a project nothing else queries. This is the same
        // authenticated demand boundary MCP search resolves through: it
        // activates the scope (kicking the first build) and answers
        // immediately when a complete generation is already ready.
        if invocation
            .code_index_schedulers
            .latest_complete_ready_for_scope(&state.scope)
            .await
            .is_some()
            && try_deferred_feedback_registration(&invocation, &project_root, &state).await
                != DeferredFeedbackAttemptV1::AwaitNextPublication
        {
            return;
        }
        loop {
            match generation_publications.recv().await {
                Ok(publication) if publication.project_root == project_root => {}
                Ok(_) => continue,
                // A lagged receiver dropped publications; one of them may have
                // been this project's, so attempt registration anyway.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
            if try_deferred_feedback_registration(&invocation, &project_root, &state).await
                != DeferredFeedbackAttemptV1::AwaitNextPublication
            {
                return;
            }
        }
    });
}

/// One deferred registration attempt, terminal unless the provider identity
/// is still unresolved.
#[derive(PartialEq, Eq)]
enum DeferredFeedbackAttemptV1 {
    Terminal,
    AwaitNextPublication,
}

async fn try_deferred_feedback_registration(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    state: &ProjectOpenDependentOwnerState,
) -> DeferredFeedbackAttemptV1 {
    match register_production_feedback_cycle(invocation, project_root, state).await {
        Ok(ProductionFeedbackCycleRegistrationV1::Registered {
            runtime,
            feedback_scope,
            lsp_input,
        }) => {
            tracing::info!(
                event = "project_open_owner_phase",
                project = %project_root.display(),
                phase = "feedback_cycle_registered",
                deferred = true,
            );
            register_advisory_owner_from_state(
                invocation,
                project_root,
                state,
                runtime,
                feedback_scope,
                lsp_input,
            )
            .await;
            DeferredFeedbackAttemptV1::Terminal
        }
        // The publication may belong to a different repository scope or the
        // identity may still be settling; keep waiting for the next
        // publication.
        Ok(ProductionFeedbackCycleRegistrationV1::SkippedUnindexed) => {
            DeferredFeedbackAttemptV1::AwaitNextPublication
        }
        Ok(ProductionFeedbackCycleRegistrationV1::SkippedWithoutGitScope { reason }) => {
            tracing::info!(
                event = "project_open_owner_phase",
                project = %project_root.display(),
                phase = "feedback_cycle_skipped",
                reason = reason,
                deferred = true,
            );
            DeferredFeedbackAttemptV1::Terminal
        }
        Err(error) => {
            tracing::warn!(
                event = "project_open_owner_phase",
                project = %project_root.display(),
                phase = "feedback_cycle_deferred_failed",
                error = %error,
            );
            DeferredFeedbackAttemptV1::Terminal
        }
    }
}

async fn register_advisory_owner_from_state(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    state: &ProjectOpenDependentOwnerState,
    feedback_cycle: Arc<crate::application::feedback::FeedbackCycleRuntime>,
    feedback_scope: FeedbackScopeV1,
    feedback_lsp_input: crate::application::feedback::FeedbackCycleLspInput,
) {
    let outcome = register_production_advisory_owner(
        invocation,
        project_root,
        state.database.clone(),
        Arc::clone(&state.session_db),
        Arc::clone(&state.graph),
        state.scope.clone(),
        state.access.clone(),
        feedback_scope,
        feedback_cycle,
        feedback_lsp_input,
        Arc::clone(&state.lsp_session_factory),
        Arc::clone(&state.scout_registry),
        state.scout_configuration.clone(),
        state.admitted_root_uri.clone(),
        state.indexed_files.clone(),
    )
    .await;
    match outcome {
        Ok(_) => tracing::info!(
            event = "project_open_owner_phase",
            project = %project_root.display(),
            phase = "advisory_owner_registered",
            deferred = true,
        ),
        Err(error) => tracing::warn!(
            event = "project_open_owner_phase",
            project = %project_root.display(),
            phase = "advisory_owner_deferred_failed",
            error = %error,
        ),
    }
}
