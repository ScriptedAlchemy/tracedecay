//! Admitted-provider attempt execution: bounded native provider processes
//! under the durable Work attempt authority.
//!
//! Every durable transition routes through
//! [`tracedecay_application::WorkAttemptService`]; this module owns only the
//! live process — spawn, bounded stream capture, the cancellation ladder, and
//! terminal evidence capture. Provider resolution is fail-closed through the
//! pinned executable-binding authority; an unresolved provider is a typed
//! availability state, never a fallback.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Mutex as ProcessMapMutex;

use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::sync::Notify;
use tracedecay_application::{
    WorkAttemptEvidenceRecordV1, WorkAttemptProviderOutcomeV1, WorkAttemptStreamChannelV1,
    WorkAttemptStreamSummaryV1, WorkProviderAvailabilityV1,
};
use tracedecay_domain::{
    WorkAttemptIdentityV1, WorkAttemptV1, WorkProviderBackendV1, WorkProviderProtocol,
};

use crate::config::work_executable_binding::{
    PinnedWorkExecutableBindingResolver, WorkExecutableBindingError, WorkExecutableBindingResolver,
};

use super::types::RegisteredWorkRuntime;
use super::work::work_background_context;
use super::{Arc, ManifestDigest, RequestContext, current_micros};

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
    channels: ProcessMapMutex<BTreeMap<String, Arc<Notify>>>,
}

impl WorkAttemptProcessRegistryV1 {
    fn key(identity: &WorkAttemptIdentityV1) -> String {
        format!(
            "{}/{}/{}",
            identity.task_id().as_str(),
            identity.run_id().as_str(),
            identity.attempt_id().as_str()
        )
    }

    /// Registers a live attempt and returns its cancellation channel, or
    /// `None` when the attempt is already owned by a live task.
    fn register(&self, identity: &WorkAttemptIdentityV1) -> Option<Arc<Notify>> {
        let mut channels = self
            .channels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = Self::key(identity);
        if channels.contains_key(&key) {
            return None;
        }
        let notify = Arc::new(Notify::new());
        channels.insert(key, Arc::clone(&notify));
        Some(notify)
    }

    fn release(&self, identity: &WorkAttemptIdentityV1) {
        self.channels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&Self::key(identity));
    }

    /// Signals the live task for this attempt, if this daemon owns one. The
    /// durable cancellation request is already persisted by the caller; a
    /// missing channel means the process is not alive here and recovery will
    /// observe the request instead.
    pub(super) fn signal_cancellation(&self, identity: &WorkAttemptIdentityV1) {
        if let Some(notify) = self
            .channels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&Self::key(identity))
        {
            notify.notify_waiters();
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
) {
    let Some(cancel) = registry.register(attempt.identity()) else {
        return;
    };
    tokio::spawn(async move {
        let identity = attempt.identity().clone();
        run_attempt(
            registered,
            Arc::clone(&registry),
            project_root,
            attempt,
            cancel,
        )
        .await;
        registry.release(&identity);
    });
}

async fn run_attempt(
    registered: RegisteredWorkRuntime,
    _registry: Arc<WorkAttemptProcessRegistryV1>,
    project_root: PathBuf,
    attempt: WorkAttemptV1,
    cancel: Arc<Notify>,
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

    match resolve_provider(&project_root, &attempt) {
        Ok(resolved) => {
            execute_provider(attempts, &context, &attempt, &resolved, cancel).await;
        }
        Err(availability) => {
            settle_unstarted(
                attempts,
                &context,
                &identity,
                &attempt,
                WorkAttemptProviderOutcomeV1::ProviderUnavailable {
                    state: availability,
                },
            );
        }
    }
}

/// A provider binding resolved and digest-verified for one attempt.
struct ResolvedProvider {
    executable: PathBuf,
    arguments: Vec<&'static str>,
}

fn resolve_provider(
    project_root: &std::path::Path,
    attempt: &WorkAttemptV1,
) -> Result<ResolvedProvider, WorkProviderAvailabilityV1> {
    let snapshot = attempt.execution().execution_snapshot();
    let arguments = match (snapshot.backend(), snapshot.protocol()) {
        (WorkProviderBackendV1::ClaudeCodeCli, WorkProviderProtocol::ClaudeStreamJson) => {
            vec!["--print", "--output-format", "stream-json", "--verbose"]
        }
        (WorkProviderBackendV1::CodexCli, WorkProviderProtocol::CodexExecJson) => {
            vec!["exec", "--json", "-"]
        }
        // The app-server protocol is not an admitted execution path in this
        // runtime; that is a typed availability state, not a fallback.
        _ => return Err(WorkProviderAvailabilityV1::Unsupported),
    };
    let configuration = crate::config::cached_runtime_configuration(project_root)
        .map_err(|_| WorkProviderAvailabilityV1::Unavailable)?;
    let resolver = PinnedWorkExecutableBindingResolver::from_configuration(&configuration)
        .map_err(availability_state)?;
    let resolved = resolver
        .resolve(
            snapshot.executable(),
            snapshot.backend(),
            snapshot.protocol(),
        )
        .map_err(availability_state)?;
    Ok(ResolvedProvider {
        executable: resolved.canonical_path().to_path_buf(),
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
fn settle_unstarted<S, P, W>(
    attempts: &tracedecay_application::WorkAttemptService<S, P, W>,
    context: &RequestContext,
    identity: &WorkAttemptIdentityV1,
    attempt: &WorkAttemptV1,
    outcome: WorkAttemptProviderOutcomeV1,
) where
    S: tracedecay_application::WorkAttemptStoragePort,
    P: tracedecay_application::WorkProjectionReadPort,
    W: tracedecay_application::WorkStoragePort,
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
        observed_at: current_micros(),
    };
    if let Err(problem) = attempts.fail_recovery(context, identity, &evidence) {
        tracing::warn!(
            task = identity.task_id().as_str(),
            ?problem,
            "work attempt provider denial could not be sealed"
        );
    }
}

async fn execute_provider<S, P, W>(
    attempts: &tracedecay_application::WorkAttemptService<S, P, W>,
    context: &RequestContext,
    attempt: &WorkAttemptV1,
    resolved: &ResolvedProvider,
    cancel: Arc<Notify>,
) where
    S: tracedecay_application::WorkAttemptStoragePort,
    P: tracedecay_application::WorkProjectionReadPort,
    W: tracedecay_application::WorkStoragePort,
{
    let identity = attempt.identity().clone();
    let envelope = attempt.execution();
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
    for key in envelope.execution_snapshot().environment_allowlist() {
        if let Ok(value) = std::env::var(key) {
            command.env(key, value);
        }
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
            );
            return;
        }
    };
    let running = match attempts.mark_running(context, &identity, attempt.requested_route().clone())
    {
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
            terminate(&mut child, TerminationSignal::Kill);
            let _ = child.wait().await;
            WorkAttemptProviderOutcomeV1::TimedOut
        }
        () = cancel.notified() => {
            cancel_ladder(attempts, context, &identity, &mut child).await
        }
    };

    let stdout = stream_summary(stdout_task.await.ok().flatten());
    let stderr = stream_summary(stderr_task.await.ok().flatten());
    let outcome = overflow_outcome(outcome, &stdout, &stderr);
    let evidence = WorkAttemptEvidenceRecordV1 {
        identity: identity.clone(),
        requested_route: running.requested_route().clone(),
        actual_route: running.actual_route().cloned(),
        outcome,
        stdout,
        stderr,
        observed_at: current_micros(),
    };
    if let Err(problem) = attempts.settle(context, &identity, &evidence) {
        tracing::warn!(
            task = identity.task_id().as_str(),
            ?problem,
            "work attempt terminal evidence could not be sealed"
        );
    }
}

/// Runs the graceful-interrupt / forced-kill cancellation ladder after the
/// durable cancellation request has been observed.
async fn cancel_ladder<S, P, W>(
    attempts: &tracedecay_application::WorkAttemptService<S, P, W>,
    context: &RequestContext,
    identity: &WorkAttemptIdentityV1,
    child: &mut tokio::process::Child,
) -> WorkAttemptProviderOutcomeV1
where
    S: tracedecay_application::WorkAttemptStoragePort,
    P: tracedecay_application::WorkProjectionReadPort,
    W: tracedecay_application::WorkStoragePort,
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

async fn read_capped(stream: Option<impl AsyncRead + Unpin>, cap: u64) -> Option<(Vec<u8>, u64)> {
    let mut stream = stream?;
    let mut retained: Vec<u8> = Vec::new();
    let mut total: u64 = 0;
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        match stream.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                total = total.saturating_add(read as u64);
                let room = usize::try_from(cap.saturating_sub(retained.len() as u64))
                    .unwrap_or(usize::MAX)
                    .min(read);
                retained.extend_from_slice(&buffer[..room]);
            }
        }
    }
    Some((retained, total))
}

fn stream_summary(captured: Option<(Vec<u8>, u64)>) -> Option<WorkAttemptStreamSummaryV1> {
    let (retained, total) = captured?;
    let digest =
        ManifestDigest::new(format!("sha256:{}", hex::encode(Sha256::digest(&retained)))).ok()?;
    Some(WorkAttemptStreamSummaryV1 {
        byte_length: total,
        truncated: total > retained.len() as u64,
        digest,
    })
}

/// A truncated stream means the provider exceeded its admitted output
/// budget; that is a typed overflow outcome, not a silent trim, unless the
/// attempt already ended in cancellation or timeout.
fn overflow_outcome(
    outcome: WorkAttemptProviderOutcomeV1,
    stdout: &Option<WorkAttemptStreamSummaryV1>,
    stderr: &Option<WorkAttemptStreamSummaryV1>,
) -> WorkAttemptProviderOutcomeV1 {
    if matches!(
        outcome,
        WorkAttemptProviderOutcomeV1::Cancelled | WorkAttemptProviderOutcomeV1::TimedOut
    ) {
        return outcome;
    }
    if stdout.as_ref().is_some_and(|summary| summary.truncated) {
        return WorkAttemptProviderOutcomeV1::StreamOverflow {
            channel: WorkAttemptStreamChannelV1::Stdout,
        };
    }
    if stderr.as_ref().is_some_and(|summary| summary.truncated) {
        return WorkAttemptProviderOutcomeV1::StreamOverflow {
            channel: WorkAttemptStreamChannelV1::Stderr,
        };
    }
    outcome
}
