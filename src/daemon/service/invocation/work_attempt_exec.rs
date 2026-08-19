//! Admitted-provider attempt execution: bounded native provider processes
//! under the durable Work attempt authority.
//!
//! Every durable transition routes through
//! [`tracedecay_application::WorkAttemptService`]; this module owns only the
//! live process — spawn, bounded stream capture, the cancellation ladder, and
//! terminal evidence capture. Provider resolution is fail-closed through the
//! pinned executable-binding authority; an unresolved provider is a typed
//! availability state, never an invented fallback.
//!
//! # The Codex app-server preference gate
//!
//! Plan 32 (`docs/plans/tracedecay-v2/32-dynamic-workflow-runtime-and-sdk.md`,
//! "Native provider execution") states: "Codex-designated work prefers the
//! configured app-server. Codex CLI is eligible only when app-server is
//! unsupported or absent before session start and the pinned Plan 20 snapshot
//! explicitly allows that fallback." The plan index adds that the fallback is
//! "reported rather than hidden".
//!
//! [`select_provider`] is that gate. Three properties hold by construction:
//!
//! * **Preference.** A `CodexAppServer` snapshot resolves the app-server
//!   binding first and runs the JSON-RPC transport when it resolves.
//! * **Bounded fallback.** The Codex CLI is only ever reached through the
//!   snapshot's own [`WorkFallbackTopology::CodexCli`] arm, which the domain
//!   already refuses to attach to a Codex-CLI-backed snapshot. A
//!   `Disabled` topology denies the attempt instead of inventing a route.
//! * **Reported, never hidden.** Whenever the preferred backend loses, the
//!   selection carries a [`WorkProviderFallbackRecordV1`] naming the preferred
//!   backend, the typed state that disqualified it, and the fallback that took
//!   over. That record is sealed into the terminal evidence, on the fallback
//!   path *and* on the path where the fallback was also refused.
//!
//! Detection is the pinned-binding probe, not a configuration guess: the
//! resolver re-canonicalizes the configured path, requires the binding to
//! declare the `(backend, protocol)` capability, reads the on-disk bytes, and
//! compares them against the snapshot's pinned artifact digest. One
//! `canonicalize` plus one hash of an already-pinned executable is cheap, and
//! it observes the real filesystem rather than trusting configuration text.
//! It is deliberately *not* a live `initialize` handshake: spawning a provider
//! to ask whether a provider can be spawned is neither cheap nor free of
//! effect, and Plan 32 pins the negotiated capability set in the snapshot for
//! exactly this reason.
//!
//! The gate runs strictly before startup. Plan 32 is explicit that "No route
//! can change after provider startup; any later eligible fallback is a new
//! revalidated attempt", so a protocol failure on a started app-server session
//! seals [`WorkAttemptProviderOutcomeV1::ProtocolFailed`] and never silently
//! re-runs on the CLI.
//!
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Mutex as ProcessMapMutex;

use tokio::io::AsyncWriteExt;
use tokio::sync::Notify;
use tracedecay_application::{
    WorkAttemptEvidenceRecordV1, WorkAttemptProviderOutcomeV1, WorkProviderAvailabilityV1,
    WorkProviderFallbackRecordV1,
};
use tracedecay_domain::configuration::TopologyPolicyDigestV1;
use tracedecay_domain::{
    ObservationSourceIdentityV1, UtcMicros, WorkArtifactId, WorkArtifactRefV1,
    WorkAttemptIdentityV1, WorkAttemptV1, WorkExecutableReference, WorkFallbackTopology,
    WorkProviderBackendV1, WorkProviderProtocol, WorkProviderRouteV1, WorktreeId,
};
use tracedecay_sessions::runtime::codex_app_server::{
    CodexAppServerCancellation, CodexAppServerLaunchReceipt, CodexAppServerSummaryConfig,
    CodexAppServerWorkExecution, run_work_with_codex_app_server,
};
use tracedecay_usecases::observability::{
    BoundedObservabilityProducerV1, WorkNoProgressObservationV1, WorkOwnerObservationResultV1,
    record_no_progress_observation, record_terminal_attempt_product_views,
    record_work_operation_resource,
};

use crate::config::work_executable_binding::{
    PinnedWorkExecutableBindingResolver, WorkExecutableBindingError, WorkExecutableBindingResolver,
};

use super::types::RegisteredWorkRuntime;
use super::work::work_background_context;
use super::{Arc, RequestContext, current_micros};

mod operation_resource;
mod provider_output;

use operation_resource::{AttemptAdmissionTimingV1, work_operation_resource_observation};
use provider_output::{overflow_outcome, provider_session, read_capped, stream_summary};

#[cfg(test)]
mod tests;

/// How long an acknowledged cancellation may run before escalation to
/// forced termination.
const CANCELLATION_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

/// Live cancellation channels for provider attempts owned by this daemon
/// process. This is runtime plumbing only — the durable cancellation request
/// lives in the attempt row, and restart recovery never consults this map.
#[derive(Default)]
pub(super) struct WorkAttemptProcessRegistryV1 {
    channels: ProcessMapMutex<BTreeMap<String, WorkAttemptProcessHolderV1>>,
}

struct WorkAttemptProcessHolderV1 {
    worktree_id: Option<WorktreeId>,
    cancellation: Arc<Notify>,
}

impl WorkAttemptProcessRegistryV1 {
    fn key(worktree_id: Option<&WorktreeId>, identity: &WorkAttemptIdentityV1) -> String {
        format!(
            "{}/{}/{}/{}",
            worktree_id.map_or("unscoped-test", WorktreeId::as_str),
            identity.task_id().as_str(),
            identity.run_id().as_str(),
            identity.attempt_id().as_str()
        )
    }

    /// Registers a live attempt and returns its cancellation channel, or
    /// `None` when the attempt is already owned by a live task.
    #[cfg(test)]
    fn register(&self, identity: &WorkAttemptIdentityV1) -> Option<Arc<Notify>> {
        self.register_entry(identity, None)
    }

    fn register_for_worktree(
        &self,
        identity: &WorkAttemptIdentityV1,
        worktree_id: &WorktreeId,
    ) -> Option<Arc<Notify>> {
        self.register_entry(identity, Some(worktree_id))
    }

    fn register_entry(
        &self,
        identity: &WorkAttemptIdentityV1,
        worktree_id: Option<&WorktreeId>,
    ) -> Option<Arc<Notify>> {
        let mut channels = self
            .channels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = Self::key(worktree_id, identity);
        if channels.contains_key(&key) {
            return None;
        }
        let notify = Arc::new(Notify::new());
        channels.insert(
            key,
            WorkAttemptProcessHolderV1 {
                worktree_id: worktree_id.cloned(),
                cancellation: Arc::clone(&notify),
            },
        );
        Some(notify)
    }

    pub(super) fn holds_attempt(
        &self,
        worktree_id: &WorktreeId,
        identity: &WorkAttemptIdentityV1,
    ) -> bool {
        self.channels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&Self::key(Some(worktree_id), identity))
            .is_some_and(|holder| holder.worktree_id.as_ref() == Some(worktree_id))
    }

    pub(super) fn holds_worktree(&self, worktree_id: &WorktreeId) -> bool {
        self.channels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .any(|holder| holder.worktree_id.as_ref() == Some(worktree_id))
    }

    #[cfg(test)]
    fn release(&self, identity: &WorkAttemptIdentityV1) {
        self.release_entry(identity, None);
    }

    fn release_for_worktree(&self, identity: &WorkAttemptIdentityV1, worktree_id: &WorktreeId) {
        self.release_entry(identity, Some(worktree_id));
    }

    fn release_entry(&self, identity: &WorkAttemptIdentityV1, worktree_id: Option<&WorktreeId>) {
        self.channels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&Self::key(worktree_id, identity));
    }

    /// Signals the live task for this attempt, if this daemon owns one. The
    /// durable cancellation request is already persisted by the caller; a
    /// missing channel means the process is not alive here and recovery will
    /// observe the request instead.
    pub(super) fn signal_cancellation(
        &self,
        worktree_id: &WorktreeId,
        identity: &WorkAttemptIdentityV1,
    ) {
        self.signal_cancellation_entry(identity, Some(worktree_id));
    }

    #[cfg(test)]
    fn signal_test_cancellation(&self, identity: &WorkAttemptIdentityV1) {
        self.signal_cancellation_entry(identity, None);
    }

    fn signal_cancellation_entry(
        &self,
        identity: &WorkAttemptIdentityV1,
        worktree_id: Option<&WorktreeId>,
    ) {
        if let Some(notify) = self
            .channels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&Self::key(worktree_id, identity))
        {
            notify.cancellation.notify_waiters();
        }
    }
}

/// Spawns the background execution task for one leased or recovery-required
/// attempt. Ownership is exclusive: if the registry already tracks the
/// attempt, the existing task keeps it.
pub(super) fn spawn_attempt_execution(
    registered: RegisteredWorkRuntime,
    registry: Arc<WorkAttemptProcessRegistryV1>,
    project_root: PathBuf,
    attempt: WorkAttemptV1,
    observability_producer: Option<Arc<BoundedObservabilityProducerV1>>,
) {
    let worktree_id = attempt.execution().worktree_id().clone();
    let Some(cancel) = registry.register_for_worktree(attempt.identity(), &worktree_id) else {
        return;
    };
    let admitted_environment =
        admitted_provider_environment(attempt.execution().execution_snapshot());
    let scheduled = std::time::Instant::now();
    tokio::spawn(async move {
        let identity = attempt.identity().clone();
        let timing = AttemptAdmissionTimingV1 {
            scheduled,
            admitted: std::time::Instant::now(),
        };
        run_attempt(
            registered.clone(),
            project_root.clone(),
            attempt,
            admitted_environment,
            cancel,
            observability_producer.clone(),
            timing,
        )
        .await;
        super::work::workflow_fan_out::reconcile_workflow_fan_out_after_attempt(
            &registered,
            Arc::clone(&registry),
            &project_root,
            &identity,
            observability_producer,
        );
        registry.release_for_worktree(&identity, &worktree_id);
    });
}

async fn run_attempt(
    registered: RegisteredWorkRuntime,
    project_root: PathBuf,
    attempt: WorkAttemptV1,
    admitted_environment: BTreeMap<String, std::ffi::OsString>,
    cancel: Arc<Notify>,
    observability_producer: Option<Arc<BoundedObservabilityProducerV1>>,
    timing: AttemptAdmissionTimingV1,
) {
    let Ok(context) = work_background_context(&registered, attempt.identity()) else {
        tracing::warn!(
            task = attempt.identity().task_id().as_str(),
            "work attempt execution could not mint a background context"
        );
        return;
    };
    let services = match registered.database.work_application_services() {
        Ok(services) => services,
        Err(error) => {
            tracing::warn!(
                task = attempt.identity().task_id().as_str(),
                ?error,
                "work attempt execution could not attach the attempt authority"
            );
            return;
        }
    };
    let attempts = services.attempts();
    let identity = attempt.identity().clone();
    // The registration-pinned work topology policy carries the concurrency
    // policy this attempt was admitted under; its canonical digest is the
    // revision a Plan 26 no-progress terminal must name.
    let topology_policy_digest = match registered.work_topology_policy.compute_digest() {
        Ok(digest) => Some(digest),
        Err(error) => {
            tracing::warn!(
                task = identity.task_id().as_str(),
                ?error,
                "work topology policy digest is unavailable; no-progress observations are skipped"
            );
            None
        }
    };

    match select_provider(&project_root, &attempt) {
        Ok(selection) => match selection.provider.protocol {
            WorkProviderProtocol::CodexAppServerJsonRpc => {
                execute_app_server(
                    attempts,
                    &context,
                    &attempt,
                    &selection,
                    &admitted_environment,
                    cancel,
                    observability_producer.as_deref(),
                    topology_policy_digest.as_ref(),
                    timing,
                )
                .await;
            }
            _ => {
                execute_provider_with_environment(
                    attempts,
                    &context,
                    &attempt,
                    &selection,
                    &admitted_environment,
                    cancel,
                    observability_producer.as_deref(),
                    topology_policy_digest.as_ref(),
                    timing,
                )
                .await;
            }
        },
        Err(denial) => {
            settle_unstarted(
                attempts,
                &context,
                &identity,
                &attempt,
                WorkAttemptProviderOutcomeV1::ProviderUnavailable {
                    state: denial.state,
                },
                denial.fallback,
                observability_producer.as_deref(),
            );
        }
    }
}

/// A provider binding resolved and digest-verified for one attempt.
struct ResolvedProvider {
    executable: PathBuf,
    /// The transport this binding speaks. `CodexAppServerJsonRpc` is driven
    /// by the reused session client rather than by the stdio spawn path.
    protocol: WorkProviderProtocol,
    arguments: Vec<&'static str>,
}

/// One provider admitted for one attempt, plus the truth about how it won.
struct ProviderSelection {
    provider: ResolvedProvider,
    /// The route the attempt actually runs on. It differs from the attempt's
    /// requested route exactly when `fallback` is `Some`.
    actual_route: WorkProviderRouteV1,
    fallback: Option<WorkProviderFallbackRecordV1>,
}

/// A refusal to start any provider for this attempt. `fallback` is present
/// when a configured fallback existed and was itself refused, so both denials
/// reach the sealed evidence.
struct ProviderDenial {
    state: WorkProviderAvailabilityV1,
    fallback: Option<WorkProviderFallbackRecordV1>,
}

impl ProviderDenial {
    const fn preferred(state: WorkProviderAvailabilityV1) -> Self {
        Self {
            state,
            fallback: None,
        }
    }
}

/// argv the stdio transport forwards for one admitted `(backend, protocol)`
/// pair. The app-server pair carries none: its `app-server` argument belongs
/// to the session client that owns that transport.
fn provider_arguments(
    backend: WorkProviderBackendV1,
    protocol: WorkProviderProtocol,
) -> Option<Vec<&'static str>> {
    match (backend, protocol) {
        (WorkProviderBackendV1::ClaudeCodeCli, WorkProviderProtocol::ClaudeStreamJson) => {
            Some(vec![
                "--print",
                "--output-format",
                "stream-json",
                "--verbose",
            ])
        }
        (WorkProviderBackendV1::CodexAppServer, WorkProviderProtocol::CodexAppServerJsonRpc) => {
            Some(Vec::new())
        }
        (WorkProviderBackendV1::CodexCli, WorkProviderProtocol::CodexExecJson) => {
            Some(vec!["exec", "--json", "-"])
        }
        // The domain pins every protocol to exactly one backend, so a crossed
        // pair cannot be admitted upstream; refusing it here keeps the gate
        // total without inventing a route.
        _ => None,
    }
}

fn select_provider(
    project_root: &std::path::Path,
    attempt: &WorkAttemptV1,
) -> Result<ProviderSelection, ProviderDenial> {
    let configuration = crate::config::cached_runtime_configuration(project_root)
        .map_err(|_| ProviderDenial::preferred(WorkProviderAvailabilityV1::Unavailable))?;
    let resolver = PinnedWorkExecutableBindingResolver::from_configuration(&configuration)
        .map_err(|error| ProviderDenial::preferred(availability_state(error)))?;
    select_with_resolver(&resolver, attempt)
}

/// The preference gate proper, over an already-built binding authority.
fn select_with_resolver<R: WorkExecutableBindingResolver>(
    resolver: &R,
    attempt: &WorkAttemptV1,
) -> Result<ProviderSelection, ProviderDenial> {
    let snapshot = attempt.execution().execution_snapshot();
    let preferred = resolve_binding(
        resolver,
        snapshot.executable(),
        snapshot.backend(),
        snapshot.protocol(),
    );
    let preferred_state = match preferred {
        Ok(provider) => {
            return Ok(ProviderSelection {
                provider,
                actual_route: snapshot.route().clone(),
                fallback: None,
            });
        }
        Err(state) => state,
    };

    // The preferred backend lost. Only the snapshot's own topology may name a
    // successor; there is no ambient discovery here.
    let (route, executable) = match snapshot.fallback() {
        WorkFallbackTopology::Disabled => return Err(ProviderDenial::preferred(preferred_state)),
        WorkFallbackTopology::CodexCli { route, executable } => (route, executable),
    };
    let mut report = WorkProviderFallbackRecordV1 {
        preferred_backend: snapshot.backend(),
        preferred_route: snapshot.route().clone(),
        preferred_state,
        fallback_backend: WorkProviderBackendV1::CodexCli,
        fallback_route: route.clone(),
        fallback_state: None,
    };
    match resolve_binding(
        resolver,
        executable,
        WorkProviderBackendV1::CodexCli,
        WorkProviderProtocol::CodexExecJson,
    ) {
        Ok(provider) => Ok(ProviderSelection {
            provider,
            actual_route: route.clone(),
            fallback: Some(report),
        }),
        Err(fallback_state) => {
            report.fallback_state = Some(fallback_state);
            Err(ProviderDenial {
                state: preferred_state,
                fallback: Some(report),
            })
        }
    }
}

/// Probes one pinned binding: capability admission, path canonicalization,
/// and a byte-exact digest match against the snapshot's pinned artifact.
fn resolve_binding<R: WorkExecutableBindingResolver>(
    resolver: &R,
    executable: &WorkExecutableReference,
    backend: WorkProviderBackendV1,
    protocol: WorkProviderProtocol,
) -> Result<ResolvedProvider, WorkProviderAvailabilityV1> {
    let arguments =
        provider_arguments(backend, protocol).ok_or(WorkProviderAvailabilityV1::Unsupported)?;
    let resolved = resolver
        .resolve(executable, backend, protocol)
        .map_err(availability_state)?;
    Ok(ResolvedProvider {
        executable: resolved.canonical_path().to_path_buf(),
        protocol,
        arguments,
    })
}

fn availability_state(error: WorkExecutableBindingError) -> WorkProviderAvailabilityV1 {
    match error {
        WorkExecutableBindingError::Absent { .. } => WorkProviderAvailabilityV1::Absent,
        WorkExecutableBindingError::Stale { .. } => WorkProviderAvailabilityV1::Stale,
        WorkExecutableBindingError::Unsupported { .. } => WorkProviderAvailabilityV1::Unsupported,
        WorkExecutableBindingError::DigestMismatch { .. } => {
            WorkProviderAvailabilityV1::DigestMismatch
        }
        WorkExecutableBindingError::Unavailable { .. } => WorkProviderAvailabilityV1::Unavailable,
    }
}

/// Seals a terminal denial for an attempt whose provider never started:
/// fence to `RecoveryRequired`, then fail recovery with the typed outcome.
fn settle_unstarted<S>(
    attempts: &tracedecay_application::WorkAttemptService<S>,
    context: &RequestContext,
    identity: &WorkAttemptIdentityV1,
    attempt: &WorkAttemptV1,
    outcome: WorkAttemptProviderOutcomeV1,
    provider_fallback: Option<WorkProviderFallbackRecordV1>,
    observability_producer: Option<&BoundedObservabilityProducerV1>,
) where
    S: tracedecay_application::WorkAttemptStoragePort,
{
    if let Err(problem) = attempts.mark_provider_unavailable(context, identity) {
        tracing::warn!(
            task = identity.task_id().as_str(),
            ?problem,
            "work attempt could not be fenced for provider unavailability"
        );
        return;
    }
    let evidence = WorkAttemptEvidenceRecordV1 {
        identity: identity.clone(),
        requested_route: attempt.requested_route().clone(),
        actual_route: None,
        outcome,
        stdout: None,
        stderr: None,
        provider_session: None,
        provider_fallback,
        observed_at: current_micros(),
    };
    match attempts.fail_recovery(context, identity, &evidence) {
        Ok(settled) => {
            let _ = record_terminal_attempt_product_views(observability_producer, &settled);
        }
        Err(problem) => {
            tracing::warn!(
                task = identity.task_id().as_str(),
                ?problem,
                "work attempt provider denial could not be sealed"
            );
        }
    }
}

/// Resolves the values of exactly the keys the durable snapshot admits.
///
/// This map exists only for one child launch: an initial spawn captures the
/// current allowed values once, while recovery resolves current values again
/// under the same admitted key policy. Plaintext values never enter the
/// durable attempt authority; credential references resolve just in time.
/// A missing allowlisted value stays absent rather than receiving an invented
/// empty replacement or an unadmitted ambient fallback.
fn admitted_provider_environment(
    snapshot: &tracedecay_domain::WorkExecutionSnapshot,
) -> BTreeMap<String, std::ffi::OsString> {
    snapshot
        .environment_allowlist()
        .iter()
        .filter_map(|key| std::env::var_os(key).map(|value| (key.clone(), value)))
        .collect()
}

async fn execute_provider_with_environment<S>(
    attempts: &tracedecay_application::WorkAttemptService<S>,
    context: &RequestContext,
    attempt: &WorkAttemptV1,
    selection: &ProviderSelection,
    admitted_environment: &BTreeMap<String, std::ffi::OsString>,
    cancel: Arc<Notify>,
    observability_producer: Option<&BoundedObservabilityProducerV1>,
    topology_policy_digest: Option<&TopologyPolicyDigestV1>,
    timing: AttemptAdmissionTimingV1,
) where
    S: tracedecay_application::WorkAttemptStoragePort,
{
    let identity = attempt.identity().clone();
    let envelope = attempt.execution();
    let resolved = &selection.provider;
    let mut command = tokio::process::Command::new(&resolved.executable);
    command
        .args(&resolved.arguments)
        .current_dir(envelope.worktree_root())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear()
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    for (key, value) in admitted_environment {
        command.env(key, value);
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            tracing::warn!(
                task = identity.task_id().as_str(),
                ?error,
                "work attempt provider process could not be spawned"
            );
            settle_unstarted(
                attempts,
                context,
                &identity,
                attempt,
                WorkAttemptProviderOutcomeV1::LaunchFailed,
                selection.fallback.clone(),
                observability_producer,
            );
            return;
        }
    };
    let running = match attempts.mark_running(context, &identity, selection.actual_route.clone()) {
        Ok(running) => running,
        Err(problem) => {
            // The lease no longer admits this task (fenced by recovery or a
            // concurrent transition). Kill the orphan and stop: the durable
            // row already tells the truth.
            tracing::warn!(
                task = identity.task_id().as_str(),
                ?problem,
                "work attempt could not be marked running; terminating provider"
            );
            terminate(&mut child, TerminationSignal::Kill);
            let _ = child.wait().await;
            return;
        }
    };
    let started = std::time::Instant::now();

    if let Some(mut stdin) = child.stdin.take() {
        let instructions = envelope.instructions().as_bytes().to_vec();
        if let Err(error) = stdin.write_all(&instructions).await {
            tracing::debug!(
                task = identity.task_id().as_str(),
                ?error,
                "work attempt provider closed stdin early"
            );
        }
        drop(stdin);
    }

    let budget = envelope.budget();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_task =
        tokio::spawn(async move { read_capped(stdout, budget.max_stdout_bytes()).await });
    let stderr_task =
        tokio::spawn(async move { read_capped(stderr, budget.max_stderr_bytes()).await });

    let deadline_micros =
        u64::try_from(envelope.deadline().0.saturating_sub(current_micros().0)).unwrap_or(0);
    let wall = std::time::Duration::from_micros(deadline_micros);

    let outcome = tokio::select! {
        status = child.wait() => match status {
            Ok(status) => exit_outcome(status),
            Err(_) => WorkAttemptProviderOutcomeV1::LaunchFailed,
        },
        () = tokio::time::sleep(wall) => {
            let stalled_for = started.elapsed();
            terminate(&mut child, TerminationSignal::Kill);
            let _ = child.wait().await;
            offer_no_progress_observation(
                observability_producer,
                &identity,
                envelope.deadline(),
                topology_policy_digest,
                deadline_micros,
                stalled_for,
            );
            WorkAttemptProviderOutcomeV1::TimedOut
        }
        () = cancel.notified() => {
            cancel_ladder(attempts, context, &identity, &mut child).await
        }
    };

    let captured_stdout = stdout_task.await.ok().flatten();
    let provider_session = provider_session(resolved.protocol, captured_stdout.as_ref());
    let stdout = stream_summary(captured_stdout);
    let stderr = stream_summary(stderr_task.await.ok().flatten());
    let mut outcome = overflow_outcome(outcome, &stdout, &stderr);
    let artifacts = match stdout
        .as_ref()
        .filter(|summary| summary.byte_length > 0 && !summary.truncated)
    {
        None => Vec::new(),
        Some(summary) => match WorkArtifactId::new("artifact.provider.stdout".to_owned()) {
            Ok(artifact_id) => match WorkArtifactRefV1::new(
                artifact_id,
                summary.digest.clone(),
                summary.byte_length,
            ) {
                Ok(artifact) => vec![artifact],
                Err(error) => {
                    tracing::warn!(
                        task = identity.task_id().as_str(),
                        ?error,
                        "work attempt provider stdout artifact could not be sealed"
                    );
                    outcome = WorkAttemptProviderOutcomeV1::ProtocolFailed;
                    Vec::new()
                }
            },
            Err(error) => {
                tracing::warn!(
                    task = identity.task_id().as_str(),
                    ?error,
                    "work attempt provider stdout identity could not be sealed"
                );
                outcome = WorkAttemptProviderOutcomeV1::ProtocolFailed;
                Vec::new()
            }
        },
    };
    let terminal = std::time::Instant::now();
    let evidence = WorkAttemptEvidenceRecordV1 {
        identity: identity.clone(),
        requested_route: running.requested_route().clone(),
        actual_route: running.actual_route().cloned(),
        outcome,
        stdout,
        stderr,
        provider_session,
        provider_fallback: selection.fallback.clone(),
        observed_at: current_micros(),
    };
    match attempts.settle_with_artifacts(context, &identity, &evidence, artifacts) {
        Ok(settled) => {
            let _ = record_terminal_attempt_product_views(observability_producer, &settled);
            if let Some(observation) =
                work_operation_resource_observation(&settled, timing, started, terminal, None)
            {
                let _ =
                    record_work_operation_resource(observability_producer, &settled, observation);
            }
        }
        Err(problem) => {
            tracing::warn!(
                task = identity.task_id().as_str(),
                ?problem,
                "work attempt terminal evidence could not be sealed"
            );
        }
    }
}

/// Which arm of the app-server execution select fired. The session's own
/// result is carried through so the terminal classification stays in one
/// place.
enum AppServerEnding {
    Session(Result<Result<AppServerSessionOutput, String>, tokio::task::JoinError>),
    TimedOut,
    Cancelled,
}

struct AppServerSessionOutput {
    answer: String,
    source: ObservationSourceIdentityV1,
    provider_request_id: Option<String>,
}

/// Runs one attempt over the Codex app-server JSON-RPC transport.
///
/// The transport itself is not reimplemented here: process spawn, the
/// `initialize` handshake, ephemeral thread lifecycle, turn collection and
/// process-tree cancellation all live in
/// `tracedecay_sessions::runtime::codex_app_server`, which the row-56 rework
/// already built for exactly this call
/// ([`run_work_with_codex_app_server`] takes the cwd, wall budget and
/// cancellation handle a Work attempt needs and had no other caller).
///
/// The session client is blocking, so it runs on a blocking worker while the
/// deadline and cancellation arms stay on the runtime — the same three-armed
/// shape the stdio path uses.
async fn execute_app_server<S>(
    attempts: &tracedecay_application::WorkAttemptService<S>,
    context: &RequestContext,
    attempt: &WorkAttemptV1,
    selection: &ProviderSelection,
    admitted_environment: &BTreeMap<String, std::ffi::OsString>,
    cancel: Arc<Notify>,
    observability_producer: Option<&BoundedObservabilityProducerV1>,
    topology_policy_digest: Option<&TopologyPolicyDigestV1>,
    timing: AttemptAdmissionTimingV1,
) where
    S: tracedecay_application::WorkAttemptStoragePort,
{
    let identity = attempt.identity().clone();
    let envelope = attempt.execution();
    let snapshot = envelope.execution_snapshot();

    // The launch and the protocol session are one indivisible blocking call
    // here, so the attempt is marked Running before it starts; a launch that
    // never happens surfaces as a typed failure on the settle path instead.
    let running = match attempts.mark_running(context, &identity, selection.actual_route.clone()) {
        Ok(running) => running,
        Err(problem) => {
            tracing::warn!(
                task = identity.task_id().as_str(),
                ?problem,
                "work attempt could not be marked running; app-server was not started"
            );
            return;
        }
    };
    let attempt_started = std::time::Instant::now();
    let deadline_micros =
        u64::try_from(envelope.deadline().0.saturating_sub(current_micros().0)).unwrap_or(0);
    let wall = std::time::Duration::from_micros(deadline_micros);
    let cancellation = CodexAppServerCancellation::default();
    let config = CodexAppServerSummaryConfig {
        codex_bin: selection.provider.executable.to_string_lossy().into_owned(),
        model: Some(snapshot.model().to_owned()),
        timeout: wall,
    };
    let prompt = envelope.instructions().to_owned();
    let cwd = PathBuf::from(envelope.worktree_root());
    let session_cancellation = cancellation.clone();
    let admitted_environment = admitted_environment.clone();
    let provider_id = running
        .actual_route()
        .map_or(attempt.requested_route().provider_id(), |route| {
            route.provider_id()
        })
        .clone();
    let launch_receipt = CodexAppServerLaunchReceipt::default();
    let blocking_launch_receipt = launch_receipt.clone();
    let mut session = tokio::task::spawn_blocking(move || {
        run_work_with_codex_app_server(
            &prompt,
            &config,
            "tracedecay_work_attempt",
            CodexAppServerWorkExecution {
                cancellation: &session_cancellation,
                cwd: &cwd,
                timeout: wall,
                admitted_environment: &admitted_environment,
                launch_receipt: &blocking_launch_receipt,
            },
        )
        .and_then(|summary| {
            let source = tracedecay_domain::ObservationSourceIdentityV1::for_provider(
                provider_id,
                tracedecay_domain::SessionId::new(summary.thread_id).map_err(|error| {
                    crate::errors::TraceDecayError::Config {
                        message: format!("Codex app-server returned an invalid thread id: {error}"),
                    }
                })?,
            )
            .map_err(|error| crate::errors::TraceDecayError::Config {
                message: format!("Codex app-server session identity is invalid: {error}"),
            })?;
            Ok(AppServerSessionOutput {
                answer: summary.text,
                source,
                provider_request_id: summary.provider_request_id,
            })
        })
        .map_err(|error| error.to_string())
    });

    // The terminal arms only decide *how* the session ended; draining the
    // blocking worker happens after the select, where the join handle is no
    // longer borrowed by it.
    let ending = tokio::select! {
        joined = &mut session => AppServerEnding::Session(joined),
        () = tokio::time::sleep(wall) => {
            let stalled_for = attempt_started.elapsed();
            // Cancelling the app-server session SIGKILLs its whole process
            // tree; the escalation observed here is the same kill rung as the
            // stdio path.
            cancellation.cancel();
            offer_no_progress_observation(
                observability_producer,
                &identity,
                envelope.deadline(),
                topology_policy_digest,
                deadline_micros,
                stalled_for,
            );
            AppServerEnding::TimedOut
        }
        () = cancel.notified() => {
            // The app-server has no graceful rung: cancelling terminates the
            // whole process tree, so the ladder acknowledges and stops there
            // rather than pretending an interrupt was survived.
            if let Err(problem) =
                attempts.acknowledge_cancellation(context, &identity, current_micros())
            {
                tracing::warn!(
                    task = identity.task_id().as_str(),
                    ?problem,
                    "work attempt cancellation could not be acknowledged"
                );
            }
            cancellation.cancel();
            AppServerEnding::Cancelled
        }
    };

    let mut text = None;
    let mut provider_session = None;
    let mut provider_request_id = None;
    let outcome = match ending {
        AppServerEnding::TimedOut => {
            let _ = session.await;
            WorkAttemptProviderOutcomeV1::TimedOut
        }
        AppServerEnding::Cancelled => {
            let _ = session.await;
            WorkAttemptProviderOutcomeV1::Cancelled
        }
        AppServerEnding::Session(Ok(Ok(output))) => {
            text = Some(output.answer);
            provider_session = Some(output.source);
            provider_request_id = output.provider_request_id;
            WorkAttemptProviderOutcomeV1::Exited { code: 0 }
        }
        AppServerEnding::Session(Ok(Err(error))) => {
            tracing::debug!(
                task = identity.task_id().as_str(),
                %error,
                "codex app-server session did not reach a terminal answer"
            );
            WorkAttemptProviderOutcomeV1::ProtocolFailed
        }
        AppServerEnding::Session(Err(_)) => WorkAttemptProviderOutcomeV1::ProtocolFailed,
    };

    let stdout = stream_summary(text.map(|answer| {
        let bytes = answer.into_bytes();
        let total = bytes.len() as u64;
        let cap = usize::try_from(envelope.budget().max_stdout_bytes()).unwrap_or(usize::MAX);
        let retained = bytes[..cap.min(bytes.len())].to_vec();
        (retained, total)
    }));
    let outcome = overflow_outcome(outcome, &stdout, &None);
    let terminal = std::time::Instant::now();
    let evidence = WorkAttemptEvidenceRecordV1 {
        identity: identity.clone(),
        requested_route: running.requested_route().clone(),
        actual_route: running.actual_route().cloned(),
        outcome,
        stdout,
        // The session client discards the app-server's stderr; there is no
        // second channel to summarize truthfully.
        stderr: None,
        provider_session,
        provider_fallback: selection.fallback.clone(),
        observed_at: current_micros(),
    };
    match attempts.settle(context, &identity, &evidence) {
        Ok(settled) => {
            let _ = record_terminal_attempt_product_views(observability_producer, &settled);
            if let Some(observation) = launch_receipt.started_at().and_then(|started| {
                work_operation_resource_observation(
                    &settled,
                    timing,
                    started,
                    terminal,
                    provider_request_id,
                )
            }) {
                let _ =
                    record_work_operation_resource(observability_producer, &settled, observation);
            }
        }
        Err(problem) => {
            tracing::warn!(
                task = identity.task_id().as_str(),
                ?problem,
                "work attempt terminal evidence could not be sealed"
            );
        }
    }
}

/// Offers Plan 26's no-progress terminal for one wall-exhausted attempt. The
/// stall is the monotonic elapsed time from attempt start to the deadline arm
/// firing; a zero armed budget is refused by the payload contract, and
/// emission never alters the timed-out product handling.
fn offer_no_progress_observation(
    observability_producer: Option<&BoundedObservabilityProducerV1>,
    identity: &WorkAttemptIdentityV1,
    run_deadline: UtcMicros,
    topology_policy_digest: Option<&TopologyPolicyDigestV1>,
    configured_timeout_micros: u64,
    stalled_for: std::time::Duration,
) {
    let Some(topology_policy_digest) = topology_policy_digest else {
        tracing::debug!(
            task = identity.task_id().as_str(),
            "work attempt no-progress observation skipped: topology policy digest unavailable"
        );
        return;
    };
    let elapsed_stall_micros = u64::try_from(stalled_for.as_micros()).unwrap_or(u64::MAX);
    let result = record_no_progress_observation(
        observability_producer,
        &WorkNoProgressObservationV1 {
            attempt: identity,
            run_deadline,
            concurrency_policy_revision: topology_policy_digest.0.as_str(),
            configured_timeout_micros,
            elapsed_stall_micros,
            observed_at: current_micros(),
        },
    );
    if result != WorkOwnerObservationResultV1::Enqueued {
        tracing::debug!(
            task = identity.task_id().as_str(),
            ?result,
            "work attempt no-progress observation was not enqueued"
        );
    }
}

/// Runs the graceful-interrupt / forced-kill cancellation ladder after the
/// durable cancellation request has been observed.
async fn cancel_ladder<S>(
    attempts: &tracedecay_application::WorkAttemptService<S>,
    context: &RequestContext,
    identity: &WorkAttemptIdentityV1,
    child: &mut tokio::process::Child,
) -> WorkAttemptProviderOutcomeV1
where
    S: tracedecay_application::WorkAttemptStoragePort,
{
    if let Err(problem) = attempts.acknowledge_cancellation(context, identity, current_micros()) {
        tracing::warn!(
            task = identity.task_id().as_str(),
            ?problem,
            "work attempt cancellation could not be acknowledged"
        );
    }
    terminate(child, TerminationSignal::Interrupt);
    if tokio::time::timeout(CANCELLATION_GRACE, child.wait())
        .await
        .is_err()
    {
        if let Err(problem) = attempts.escalate_cancellation(context, identity, current_micros()) {
            tracing::warn!(
                task = identity.task_id().as_str(),
                ?problem,
                "work attempt cancellation could not be escalated"
            );
        }
        terminate(child, TerminationSignal::Kill);
        let _ = child.wait().await;
    }
    WorkAttemptProviderOutcomeV1::Cancelled
}

/// Cancellation-ladder rung, kept platform-neutral so call sites compile on
/// every target; only the unix `terminate` maps it onto a real signal number.
#[derive(Clone, Copy)]
enum TerminationSignal {
    Interrupt,
    Kill,
}

#[cfg(unix)]
fn terminate(child: &mut tokio::process::Child, signal: TerminationSignal) {
    let signal = match signal {
        TerminationSignal::Interrupt => libc::SIGINT,
        TerminationSignal::Kill => libc::SIGKILL,
    };
    if let Some(pid) = child.id() {
        let pid = pid as libc::pid_t;
        // The child leads its own process group; signal the whole group so
        // provider-spawned descendants observe the ladder too.
        // SAFETY: killpg with a pid owned by this daemon and a constant
        // signal number has no memory-safety obligations.
        if unsafe { libc::killpg(pid, signal) } != 0 {
            let _ = child.start_kill();
        }
    }
}

#[cfg(not(unix))]
fn terminate(child: &mut tokio::process::Child, _signal: TerminationSignal) {
    let _ = child.start_kill();
}

#[cfg(unix)]
fn exit_outcome(status: std::process::ExitStatus) -> WorkAttemptProviderOutcomeV1 {
    use std::os::unix::process::ExitStatusExt;

    match (status.code(), status.signal()) {
        (Some(code), _) => WorkAttemptProviderOutcomeV1::Exited { code },
        (None, Some(signal)) => WorkAttemptProviderOutcomeV1::Signalled { signal },
        (None, None) => WorkAttemptProviderOutcomeV1::LaunchFailed,
    }
}

#[cfg(not(unix))]
fn exit_outcome(status: std::process::ExitStatus) -> WorkAttemptProviderOutcomeV1 {
    match status.code() {
        Some(code) => WorkAttemptProviderOutcomeV1::Exited { code },
        None => WorkAttemptProviderOutcomeV1::LaunchFailed,
    }
}
