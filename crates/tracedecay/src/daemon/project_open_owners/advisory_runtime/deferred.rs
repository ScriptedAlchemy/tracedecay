use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::{broadcast, watch};
use tracedecay_code_index_runtime::code_index_scheduler::CodeIndexGenerationPublishedV1;

use super::super::{
    install_semantic_activation_runtime_owner, project_open_lsp_scope_grant,
    register_production_lsp_owner,
};
use super::{
    DaemonInvocationState, ProjectOpenDependentOwnerState, register_production_advisory_owner,
    register_production_feedback_and_advisory, register_production_feedback_cycle,
    selected_feedback_generation,
};
use tracedecay_contracts::doctor::{
    SemanticOwnerDegradedReasonV1, SemanticOwnerPrerequisiteV1, SemanticOwnerStateV1,
};
use tracedecay_contracts::now_micros;
use tracedecay_daemon_service::ProjectRuntimePublicationAttemptV1;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_runtime_core::logging::log_daemon_event;

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

fn pending_semantic_owner_state(
    configuration_ready: bool,
    production_runtime_ready: bool,
) -> SemanticOwnerStateV1 {
    let mut missing = Vec::new();
    if !configuration_ready {
        missing.push(SemanticOwnerPrerequisiteV1::ConfigurationRuntime);
    }
    if !production_runtime_ready {
        missing.push(SemanticOwnerPrerequisiteV1::ProductionSemanticRuntime);
    }
    SemanticOwnerStateV1::PendingPrerequisites { missing }
}

fn cancelled_semantic_owner_state(detail: &'static str) -> SemanticOwnerStateV1 {
    SemanticOwnerStateV1::Degraded {
        reason: SemanticOwnerDegradedReasonV1::TaskCancelled,
        detail: detail.to_owned(),
    }
}

pub(in crate::daemon) async fn spawn_semantic_owner_registration(
    invocation: DaemonInvocationState,
    project_root: PathBuf,
    configuration_runtime: Arc<tracedecay_configuration::ProjectConfigurationRuntime>,
    scope: tracedecay_contracts::ResolvedScope,
    mut production_runtime_ready: tokio::sync::watch::Receiver<bool>,
    route_registered: Arc<AtomicBool>,
    route_cancellation: CancellationToken,
) -> Result<ProjectRuntimePublicationAttemptV1> {
    let registration = invocation
        .semantic_owner_runtime_registrar()
        .register(&project_root)
        .await?;
    let publication = invocation
        .service
        .project_runtimes
        .begin_publication(&project_root)
        .ok_or_else(|| TraceDecayError::Config {
            message: "semantic owner disappeared before project publication began".to_owned(),
        })?;
    let task_signals = registration.signals();
    let mut configuration_ready = task_signals.subscribe_configuration_ready();
    let task_cancellation = task_signals.cancellation();
    let task_project_root = project_root.clone();
    let task_invocation = invocation.clone();
    let spawned = registration.spawn(hotpath::future!(
        async move {
            loop {
                if task_cancellation.is_cancelled() {
                    task_signals.set_state(cancelled_semantic_owner_state(
                        "semantic owner registration was cancelled by project shutdown",
                    ));
                    return;
                }
                if route_cancellation.is_cancelled() || !route_registered.load(Ordering::Acquire) {
                    task_signals.set_state(cancelled_semantic_owner_state(
                        "semantic owner registration was cancelled with its project route",
                    ));
                    return;
                }
                let configuration_is_ready = *configuration_ready.borrow_and_update();
                let production_is_ready = *production_runtime_ready.borrow_and_update();
                if configuration_is_ready && production_is_ready {
                    match install_semantic_activation_runtime_owner(
                        &task_invocation,
                        &task_project_root,
                        Arc::clone(&configuration_runtime),
                        scope.clone(),
                    )
                    .await
                    {
                        Ok(true) => {
                            if task_cancellation.is_cancelled()
                                || route_cancellation.is_cancelled()
                                || !route_registered.load(Ordering::Acquire)
                            {
                                task_signals.set_state(cancelled_semantic_owner_state(
                                    "semantic owner registration completed after its project route was cancelled",
                                ));
                                return;
                            }
                            task_signals.set_state(SemanticOwnerStateV1::Ready);
                            log_daemon_event(
                                "semantic_owner_registration",
                                &[
                                    ("project", task_project_root.display().to_string()),
                                    ("outcome", "ready".to_owned()),
                                ],
                            );
                            return;
                        }
                        Ok(false) => {
                            task_signals.set_state(pending_semantic_owner_state(true, false));
                        }
                        Err(error) => {
                            let state = error.into_state();
                            log_daemon_event(
                                "semantic_owner_registration",
                                &[
                                    ("project", task_project_root.display().to_string()),
                                    ("outcome", "degraded".to_owned()),
                                    ("state", format!("{state:?}")),
                                ],
                            );
                            task_signals.set_state(state);
                            return;
                        }
                    }
                } else {
                    task_signals.set_state(pending_semantic_owner_state(
                        configuration_is_ready,
                        production_is_ready,
                    ));
                }
                tokio::select! {
                    biased;
                    () = task_cancellation.cancelled() => {
                        task_signals.set_state(cancelled_semantic_owner_state(
                            "semantic owner registration was cancelled by project shutdown",
                        ));
                        return;
                    }
                    () = route_cancellation.cancelled() => {
                        task_signals.set_state(cancelled_semantic_owner_state(
                            "semantic owner registration was cancelled with its project route",
                        ));
                        return;
                    }
                    changed = configuration_ready.changed() => {
                        if changed.is_err() {
                            task_signals.set_state(cancelled_semantic_owner_state(
                                "semantic owner configuration readiness authority closed",
                            ));
                            return;
                        }
                    }
                    changed = production_runtime_ready.changed() => {
                        if changed.is_err() {
                            task_signals.set_state(cancelled_semantic_owner_state(
                                "semantic owner production-runtime readiness authority closed",
                            ));
                            return;
                        }
                    }
                }
            }
        },
        label = "daemon.project.owners.semantic_deferred"
    ));
    if spawned {
        log_daemon_event(
            "semantic_owner_registration",
            &[
                ("project", project_root.display().to_string()),
                ("outcome", "scheduled".to_owned()),
            ],
        );
    }
    Ok(publication)
}

pub(super) fn spawn(
    owner: &crate::mcp::McpServer,
    invocation: DaemonInvocationState,
    project_root: PathBuf,
    mut state: ProjectOpenDependentOwnerState,
) -> bool {
    // Nothing user-facing may wait on a layer this route disables by contract.
    // With no code index there is no generation to defer to, so the wait below
    // has no terminal state of its own: name it here instead.
    if tracedecay_code_index_runtime::project_reads::code_index_disabled_for_scope(
        &invocation.code_index_schedulers,
        &state.scope,
    ) {
        log_deferred_attempt(&project_root, "code_index_disabled", "terminal");
        return false;
    }
    // Project-open schedules this owner before code-index activation. Capture
    // the registry-wide seat cursor now so a retained generation seated before
    // the background task's first poll remains observable. The exact project
    // and scope are still revalidated by `try_mount` after every wake.
    let mut serving_seats = invocation.code_index_schedulers.subscribe_serving_seats();
    owner.spawn_background_task(hotpath::future!(
        async move {
            let mut publications = invocation
                .code_index_schedulers
                .subscribe_generation_publications();
            let mut serving_changes = None;
            let mut partial_publication_retried = false;
            loop {
                // Sealing announces durable source before the complete serving
                // owner is installed. Subscribe before probing that owner so a
                // later serving swap can finish this mount without another edit.
                if serving_changes.is_none() {
                    serving_changes = invocation
                        .code_index_schedulers
                        .subscribe_serving_generation_changes(&project_root)
                        .await;
                    if serving_changes.is_some() {
                        let _ = invocation
                            .code_index_schedulers
                            .request_complete_generation(&project_root)
                            .await;
                    }
                }
                match try_mount(&invocation, &project_root, &mut state).await {
                    Attempt::Terminal => return,
                    Attempt::RetryPartialPublication if !partial_publication_retried => {
                        partial_publication_retried = true;
                        tokio::task::yield_now().await;
                        continue;
                    }
                    Attempt::RetryPartialPublication => return,
                    Attempt::AwaitNextPublication => {
                        tracing::info!(
                            event = "advisory_deferred_generation_unavailable",
                            project = %project_root.display(),
                            serving_watch_registered = serving_changes.is_some(),
                            "waiting after exact complete-generation admission declined"
                        );
                    }
                }
                if !wait_for_generation_change(
                    &project_root,
                    &mut publications,
                    &mut serving_changes,
                    &mut serving_seats,
                )
                .await
                {
                    return;
                }
                partial_publication_retried = false;
            }
        },
        label = "daemon.project.owners.advisory_deferred"
    ))
}

async fn wait_for_generation_change(
    project_root: &Path,
    publications: &mut broadcast::Receiver<CodeIndexGenerationPublishedV1>,
    serving_changes: &mut Option<watch::Receiver<()>>,
    serving_seats: &mut watch::Receiver<u64>,
) -> bool {
    loop {
        tokio::select! {
            publication = publications.recv() => match publication {
                Ok(publication) if publication.project_root == project_root => return true,
                Ok(_) => {},
                Err(broadcast::error::RecvError::Lagged(_)) => return true,
                Err(broadcast::error::RecvError::Closed) => return false,
            },
            serving = async {
                match serving_changes.as_mut() {
                    Some(changes) => changes.changed().await,
                    None => std::future::pending().await,
                }
            } => return serving.is_ok(),
            seat = serving_seats.changed() => return seat.is_ok(),
        }
    }
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
            Ok(()) => Attempt::Terminal,
            Err(_) => classify_failure(invocation, project_root, state).await,
        };
    }
    // The shared selection ladder prevents passive waiting from deadlocking a
    // fresh project. Its decoded-for-root-scope probe is the cheap arm: it
    // reads an already-seated complete generation and asks the scheduler for
    // nothing. When nothing is seated it answers `None` and demands nothing,
    // so a deferred owner that only ever took this arm waited for a
    // publication that only demand produces — the project then served
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
    // holds, not on the session factory — only the advisory owner needs that.
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
            log_deferred_attempt(project_root, "lsp_owner_failed", &error.to_string());
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
    let attempt = if invocation
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
    } else if invocation
        .code_index_schedulers
        .latest_complete_ready(project_root)
        .await
        .is_none()
    {
        // `try_mount` also admits the recovered text-serving level, but the
        // feedback cycle it then composes mints its provider identity through
        // `ProductionFeedbackDocumentIdentityPort`, which serves only
        // `latest_complete_ready` for the exact root. When the text projection
        // is ahead of that authority the composition fails with "project-open
        // provider code-index identity is inconsistent with the application
        // contract" — earliness, not a missing composition. Classifying it
        // terminal abandoned the upgrade for the daemon's whole life: the
        // project kept the typed-unavailable feedback cycle and the warming
        // LSP owner that advertises no analyzer method at all.
        Attempt::AwaitNextPublication
    } else {
        // A serving generation exists and no feedback cycle was published, so
        // the composition itself is missing rather than early. Nothing retries
        // this owner after it returns, so name that terminal state here rather
        // than leave a project serving without a cycle and no evidence why.
        Attempt::Terminal
    };
    log_deferred_attempt(
        project_root,
        "classified_failure",
        match attempt {
            Attempt::Terminal => "terminal",
            Attempt::AwaitNextPublication => "await_next_publication",
            Attempt::RetryPartialPublication => "retry_partial_publication",
        },
    );
    attempt
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::task::{Context, Poll, Waker};

    use tracedecay_domain::{CodeGenerationId, ContentDigest, RepositoryId};

    use super::{CodeIndexGenerationPublishedV1, broadcast, wait_for_generation_change, watch};

    #[tokio::test]
    async fn serving_installation_wakes_after_sealed_publication_was_consumed() {
        let root = tempfile::tempdir().expect("project root");
        let foreign = tempfile::tempdir().expect("foreign project root");
        let (publication_sender, mut publications) = broadcast::channel(4);
        let (serving_sender, serving_receiver) = watch::channel(());
        let mut serving_changes = Some(serving_receiver);
        let (_seat_sender, mut serving_seats) = watch::channel(0_u64);
        let publication = CodeIndexGenerationPublishedV1 {
            project_root: root.path().to_path_buf(),
            repository_id: RepositoryId::new("repository.deferred").expect("repository"),
            generation_id: CodeGenerationId::new("generation.deferred").expect("generation"),
            snapshot_content_identity: ContentDigest::new(format!("sha256:{}", "a".repeat(64)))
                .expect("content digest"),
            observation_time_micros: 1,
        };
        publication_sender
            .send(publication.clone())
            .expect("sealed publication");
        assert!(
            wait_for_generation_change(
                root.path(),
                &mut publications,
                &mut serving_changes,
                &mut serving_seats,
            )
            .await
        );

        let mut waiting = Box::pin(wait_for_generation_change(
            root.path(),
            &mut publications,
            &mut serving_changes,
            &mut serving_seats,
        ));
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(waiting.as_mut().poll(&mut context), Poll::Pending));
        publication_sender
            .send(CodeIndexGenerationPublishedV1 {
                project_root: foreign.path().to_path_buf(),
                ..publication
            })
            .expect("foreign publication");
        assert!(matches!(waiting.as_mut().poll(&mut context), Poll::Pending));

        serving_sender.send_replace(());
        assert!(matches!(
            waiting.as_mut().poll(&mut context),
            Poll::Ready(true)
        ));
        drop(waiting);
        drop(serving_sender);
        assert!(
            !wait_for_generation_change(
                root.path(),
                &mut publications,
                &mut serving_changes,
                &mut serving_seats,
            )
            .await
        );
    }

    #[tokio::test]
    async fn retained_seat_wakes_after_subscription_precedes_scheduler_enrollment() {
        let root = tempfile::tempdir().expect("project root");
        let (publication_sender, mut publications) = broadcast::channel(1);
        let (seat_sender, mut serving_seats) = watch::channel(0_u64);

        // The deferred owner subscribes while no per-project scheduler exists.
        // A retained generation then seats without a new-generation broadcast.
        seat_sender.send_modify(|seats| *seats += 1);
        assert!(
            wait_for_generation_change(
                root.path(),
                &mut publications,
                &mut None,
                &mut serving_seats,
            )
            .await
        );
        drop(publication_sender);
    }

    #[tokio::test]
    async fn publication_channel_closure_stops_a_wait_before_scheduler_mount() {
        let root = tempfile::tempdir().expect("project root");
        let (sender, mut publications) = broadcast::channel(1);
        let (_seat_sender, mut serving_seats) = watch::channel(0_u64);
        drop(sender);
        assert!(
            !wait_for_generation_change(
                root.path(),
                &mut publications,
                &mut None,
                &mut serving_seats,
            )
            .await
        );
    }
}
