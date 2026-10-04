use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::watch;
use tracedecay_code_index_runtime::code_index_scheduler::CodeIndexOwnerSignalsV1;

use super::super::{project_open_lsp_scope_grant, register_production_lsp_owner};
use super::{
    DaemonInvocationState, ProjectOpenDependentOwnerState, register_production_advisory_owner,
    register_production_feedback_and_advisory, register_production_feedback_cycle,
    selected_feedback_generation,
};
use tracedecay_contracts::{
    ApplicationProblem, ApplicationUnavailableClassV1, LegalAction, RetryDirective, SafeDiagnostic,
    now_micros,
};
use tracedecay_runtime_core::logging::log_daemon_event;

/// What the project-open advisory mount is doing for its checkout. The
/// pre-mount placeholder owner answers from this state alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::daemon::project_open_owners) enum AdvisoryMountStateV1 {
    /// No generation this mount could admit exists yet.
    AwaitingGeneration,
    /// A sealed generation, or a retained one awaiting its source proof, is
    /// being mounted.
    Mounting,
    /// The full advisory owner replaced the placeholder.
    Mounted,
    Failed(AdvisoryMountFailureV1),
}

/// Why the advisory mount ended without publishing the full owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::daemon::project_open_owners) enum AdvisoryMountFailureV1 {
    CodeIndexDisabled,
    FeedbackCycle,
    LspScopeGrant,
    LspOwner,
    AdvisoryOwner,
    /// The mount task stopped (daemon shutdown or a closed generation
    /// channel) before it reached a verdict.
    Abandoned,
}

impl AdvisoryMountFailureV1 {
    pub(in crate::daemon::project_open_owners) fn problem(self) -> ApplicationProblem {
        let message = match self {
            Self::CodeIndexDisabled => {
                "The code index was disabled when this checkout opened, so the advisory feedback cycle never mounted"
            }
            Self::FeedbackCycle => {
                "The advisory feedback cycle could not mount its feedback cycle over the sealed code-index generation"
            }
            Self::LspScopeGrant => {
                "The advisory feedback cycle could not obtain its language-server workspace grant"
            }
            Self::LspOwner => {
                "The advisory feedback cycle could not mount its language-server owner"
            }
            Self::AdvisoryOwner => {
                "The advisory feedback cycle owner could not be published for this checkout"
            }
            Self::Abandoned => {
                "The advisory feedback cycle mount stopped before publishing its owner"
            }
        };
        ApplicationProblem::Unavailable {
            classification: ApplicationUnavailableClassV1::Authority,
            diagnostic: SafeDiagnostic {
                code: "feedback.advisory-cycle.mount-failed".to_owned(),
                message: message.to_owned(),
            },
            retry: RetryDirective::Never,
            legal_actions: vec![LegalAction::ContactAdministrator],
            detail: None,
        }
    }
}

/// The advisory mount's sole write handle on its published state. Dropping it
/// before a verdict publishes `Failed(Abandoned)`, so no placeholder waits on
/// a mount that no longer runs.
pub(in crate::daemon::project_open_owners) struct AdvisoryMountPublisherV1(
    watch::Sender<AdvisoryMountStateV1>,
);

impl AdvisoryMountPublisherV1 {
    pub(in crate::daemon::project_open_owners) fn channel()
    -> (Self, watch::Receiver<AdvisoryMountStateV1>) {
        let (sender, receiver) = watch::channel(AdvisoryMountStateV1::Mounting);
        (Self(sender), receiver)
    }

    pub(in crate::daemon::project_open_owners) fn publish(&self, state: AdvisoryMountStateV1) {
        self.0.send_replace(state);
    }
}

impl Drop for AdvisoryMountPublisherV1 {
    fn drop(&mut self) {
        self.0.send_if_modified(|state| {
            let open = matches!(
                state,
                AdvisoryMountStateV1::AwaitingGeneration | AdvisoryMountStateV1::Mounting
            );
            if open {
                *state = AdvisoryMountStateV1::Failed(AdvisoryMountFailureV1::Abandoned);
            }
            open
        });
    }
}

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
    publisher: AdvisoryMountPublisherV1,
) -> bool {
    // Nothing user-facing may wait on a layer this route disables by contract.
    // With no code index there is no generation to defer to, so the wait below
    // has no terminal state of its own: name it here instead.
    if tracedecay_code_index_runtime::project_reads::code_index_disabled_for_scope(
        &invocation.code_index_schedulers,
        &state.scope,
    ) {
        log_deferred_attempt(&project_root, "code_index_disabled", "terminal");
        publisher.publish(AdvisoryMountStateV1::Failed(
            AdvisoryMountFailureV1::CodeIndexDisabled,
        ));
        return false;
    }
    owner.spawn_background_task(tracing::Instrument::instrument(
        async move {
            // Project-open schedules this owner before code-index activation,
            // and sealing announces durable source before its text owner is
            // installed. Subscribe before the first probe so a generation
            // seated or installed after it still wakes this mount; the exact
            // project and scope are revalidated by `try_mount` after every wake.
            let mut signals = CodeIndexOwnerSignalsV1::subscribe(
                &invocation.code_index_schedulers,
                &project_root,
            )
            .await;
            let mut partial_publication_retried = false;
            let settled = loop {
                publisher.publish(AdvisoryMountStateV1::Mounting);
                match try_mount(&invocation, &project_root, &mut state).await {
                    Attempt::Settled(settled) => break settled,
                    Attempt::RetryPartialPublication if !partial_publication_retried => {
                        partial_publication_retried = true;
                        tokio::task::yield_now().await;
                        continue;
                    }
                    Attempt::RetryPartialPublication => {
                        break AdvisoryMountStateV1::Failed(AdvisoryMountFailureV1::AdvisoryOwner);
                    }
                    Attempt::AwaitNextPublication => {
                        let awaiting = awaiting_state(&invocation, &state.scope).await;
                        publisher.publish(awaiting);
                        tracing::info!(
                            event = "advisory_deferred_generation_unavailable",
                            project = %project_root.display(),
                            state = ?awaiting,
                            "waiting after exact complete-generation admission declined"
                        );
                    }
                }
                if signals.changed().await.is_err() {
                    return;
                }
                partial_publication_retried = false;
            };
            publisher.publish(settled);
        },
        tracing::trace_span!("daemon.project.owners.advisory_deferred"),
    ))
}

/// A retained generation's pending source proof, or the successor it finds
/// owed, publishes the generation this mount then admits, so that wait is
/// still a mount in progress rather than a wait for a first generation.
async fn awaiting_state(
    invocation: &DaemonInvocationState,
    scope: &tracedecay_contracts::ResolvedScope,
) -> AdvisoryMountStateV1 {
    invocation
        .code_index_schedulers
        .retained_text_owner_freshness_for_scope(scope)
        .await
        .map_or(AdvisoryMountStateV1::AwaitingGeneration, |_| {
            AdvisoryMountStateV1::Mounting
        })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Attempt {
    /// `Mounted` or `Failed`: nothing retries this owner after it returns.
    Settled(AdvisoryMountStateV1),
    AwaitNextPublication,
    RetryPartialPublication,
}

#[tracing::instrument(
    name = "daemon.project.owners.advisory_retry",
    level = "trace",
    skip_all
)]
#[expect(
    clippy::too_many_lines,
    reason = "Deferred advisory mount is one generation-ready attach of the feedback and LSP owners."
)]
async fn try_mount(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    state: &mut ProjectOpenDependentOwnerState,
) -> Attempt {
    if let Some(lsp_session_factory) = state.lsp_session_factory.clone() {
        let Some(indexed_generation) =
            selected_feedback_generation(invocation, project_root, &state.scope).await
        else {
            return Attempt::AwaitNextPublication;
        };
        return match register_production_feedback_and_advisory(
            invocation,
            project_root,
            state,
            lsp_session_factory,
            indexed_generation,
        )
        .await
        {
            Ok(()) => Attempt::Settled(AdvisoryMountStateV1::Mounted),
            Err(_) => classify_failure(invocation, project_root, state).await,
        };
    }
    // The shared selection ladder prevents passive waiting from deadlocking a
    // fresh project. Its decoded-for-root-scope probe is the cheap arm: it
    // reads an already-seated complete generation and asks the scheduler for
    // nothing. When nothing is seated it answers `None` and demands nothing,
    // so a deferred owner that only ever took this arm waited for a
    // publication that only demand produces, the project then served
    // indefinitely with the typed-unavailable feedback cycle.
    // Its demand and recovered-text arms both require current source evidence
    // before this owner may admit their generation.
    let indexed = invocation
        .code_index_schedulers
        .latest_feedback_generation_for_scope(project_root, &state.scope)
        .await;
    // Deliberately unlogged: the poll in `spawn` re-enters here once a second
    // while a cold project indexes, and one event per second per warming
    // project is noise, not evidence. `spawn` records the wait once instead.
    let Some(indexed) = indexed else {
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
    // holds, not on the session factory, only the advisory owner needs that.
    let (feedback_cycle, feedback_scope) =
        match register_production_feedback_cycle(invocation, project_root, state, indexed.clone())
            .await
        {
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
            log_deferred_attempt(project_root, "lsp_scope_grant_failed", &error.to_string());
            return Attempt::Settled(AdvisoryMountStateV1::Failed(
                AdvisoryMountFailureV1::LspScopeGrant,
            ));
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
            log_deferred_attempt(project_root, "lsp_owner_failed", &error.to_string());
            return Attempt::Settled(AdvisoryMountStateV1::Failed(
                AdvisoryMountFailureV1::LspOwner,
            ));
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
    Attempt::Settled(AdvisoryMountStateV1::Mounted)
}

async fn classify_failure(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    state: &ProjectOpenDependentOwnerState,
) -> Attempt {
    let attempt = if invocation
        .service
        .feedback_cycle(Some(project_root))
        .await
        .is_some()
    {
        Attempt::RetryPartialPublication
    } else if invocation
        .code_index_schedulers
        .latest_feedback_generation_for_scope(project_root, &state.scope)
        .await
        .is_none()
    {
        // The feedback cycle mints its provider identity from the same
        // selection. While it answers nothing for the exact root the
        // composition failed early, not for want of a composition; a
        // terminal verdict here abandoned the upgrade for the daemon's life.
        Attempt::AwaitNextPublication
    } else {
        // A serving generation exists and no feedback cycle was published, so
        // the composition itself is missing rather than early. Nothing retries
        // this owner after it returns, so name that terminal state here rather
        // than leave a project serving without a cycle and no evidence why.
        Attempt::Settled(AdvisoryMountStateV1::Failed(
            AdvisoryMountFailureV1::FeedbackCycle,
        ))
    };
    log_deferred_attempt(
        project_root,
        "classified_failure",
        match attempt {
            Attempt::Settled(_) => "terminal",
            Attempt::AwaitNextPublication => "await_next_publication",
            Attempt::RetryPartialPublication => "retry_partial_publication",
        },
    );
    attempt
}
