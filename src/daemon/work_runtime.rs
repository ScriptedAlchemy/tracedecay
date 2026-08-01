use std::fmt::Display;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;

use tracedecay_application::{
    WorkAttemptAcquireLeaseRequestV1, WorkAttemptPersistencePort, WorkAttemptResponseV1,
    WorkDispatchBoundsV1, WorkDispatchError, WorkExecutionError, WorkExecutionQueueV1,
    WorkExecutionService, WorkProviderExecutionError, WorkProviderSettlementV1, WorkStorageError,
    WorkStoragePort,
};
#[cfg(all(test, unix))]
use tracedecay_domain::WorkProviderRouteV1;
use tracedecay_domain::{
    AttemptId, ManifestDigest, UtcMicros, WorkArtifactId, WorkArtifactRefV1, WorkAttemptIdentityV1,
    WorkAttemptProgressV1, WorkAttemptProjectionBindingV1, WorkAttemptStateV1, WorkAttemptV1,
    WorkAuthority, WorkCancellationAcknowledgementV1, WorkCancellationRequestV1,
    WorkCancellationStateV1, WorkExecutionEnvelopeV1, WorkLeaseFenceV1, WorkProjectionSnapshotV1,
    WorkRecoveryStateV1, WorkRestartReasonV1, WorkTerminalEvidenceV1, canonical_sha256,
};

use crate::application::event_lane::{self, ActivityFamilyV1};
use crate::daemon_contract::WorkAttemptInvocationV1;
use crate::global_db::RegisteredGlobalDb;
use crate::sessions::codex_app_server::CodexAppServerSummaryConfig;

mod codex_provider;
mod native_cli;
#[cfg(all(test, unix))]
mod tests;

#[cfg(test)]
pub(crate) use codex_provider::CODEX_PROVIDER_ID;
use codex_provider::{NativeWorkProviderConfigV1, NativeWorkProviderV1};

/// Provider executions one daemon project runtime may run at once.
const DEFAULT_WORK_EXECUTION_CAPACITY: usize = 4;

pub(crate) struct DaemonWorkRuntimeV1<S>
where
    S: WorkAttemptPersistencePort + WorkStoragePort + Clone + Send + Sync + 'static,
{
    authority: WorkAuthority,
    storage: S,
    queue: Arc<WorkExecutionQueueV1<NativeWorkProviderV1<S>>>,
    execution: WorkExecutionService<S>,
    observation_db: Arc<RegisteredGlobalDb>,
    project_root: PathBuf,
    configuration_digest: ManifestDigest,
}

impl<S> DaemonWorkRuntimeV1<S>
where
    S: WorkAttemptPersistencePort + WorkStoragePort + Clone + Send + Sync + 'static,
{
    pub(crate) fn new(
        authority: WorkAuthority,
        storage: S,
        config: CodexAppServerSummaryConfig,
        configuration_digest: ManifestDigest,
        observation_db: Arc<RegisteredGlobalDb>,
        project_root: PathBuf,
    ) -> Self {
        Self::with_capacity(
            authority,
            storage,
            config,
            configuration_digest,
            observation_db,
            project_root,
            NonZeroUsize::new(DEFAULT_WORK_EXECUTION_CAPACITY).unwrap_or(NonZeroUsize::MIN),
        )
    }

    pub(crate) fn with_capacity(
        authority: WorkAuthority,
        storage: S,
        config: CodexAppServerSummaryConfig,
        configuration_digest: ManifestDigest,
        observation_db: Arc<RegisteredGlobalDb>,
        project_root: PathBuf,
        capacity: NonZeroUsize,
    ) -> Self {
        let provider = NativeWorkProviderV1::new(
            storage.clone(),
            authority.clone(),
            NativeWorkProviderConfigV1::from_registered(
                config,
                configuration_digest.clone(),
                project_root.clone(),
            ),
        );
        Self {
            authority,
            storage: storage.clone(),
            queue: Arc::new(WorkExecutionQueueV1::new(
                provider,
                WorkDispatchBoundsV1::new(capacity),
            )),
            execution: WorkExecutionService::new(storage),
            observation_db,
            project_root,
            configuration_digest,
        }
    }

    #[cfg(all(test, unix))]
    pub(crate) fn provider_route(&self) -> Result<WorkProviderRouteV1, WorkProviderExecutionError> {
        self.queue.route()
    }

    #[cfg(all(test, unix))]
    pub(crate) fn is_ready(&self) -> bool {
        self.queue.provider().is_ready() && event_lane::enabled(Some(self.observation_db.as_ref()))
    }

    #[cfg(all(test, unix))]
    pub(crate) fn in_flight(&self) -> usize {
        self.queue.in_flight()
    }

    pub(crate) fn capacity(&self) -> usize {
        self.queue.bounds().capacity()
    }

    /// Stops and joins every execution before the runtime is dropped.
    pub(crate) fn shutdown(&self) -> usize {
        self.queue.reap()
    }

    pub(crate) async fn dispatch(
        &self,
        request: WorkAttemptInvocationV1,
    ) -> Result<WorkAttemptResponseV1, WorkExecutionError> {
        let attempt = match request {
            WorkAttemptInvocationV1::AcquireLease(request) => {
                let request = *request;
                if !self.queue.supports_route(&request.requested_route)? {
                    return Err(WorkProviderExecutionError::Rejected(
                        "requested Work provider route is not mounted".to_owned(),
                    )
                    .into());
                }
                let binding = self.binding(&request.snapshot, request.identity.task_id())?;
                if request.projection_binding != binding {
                    return Err(WorkProviderExecutionError::Rejected(
                        "requested Work projection binding is not current".to_owned(),
                    )
                    .into());
                }
                self.acquire_lease(
                    &request.snapshot,
                    request.identity,
                    request.execution,
                    request.lease,
                )
                .await?
            }
            WorkAttemptInvocationV1::RenewLease(request) => {
                self.renew_lease(&request.identity, &request.expected, request.replacement)?
            }
            WorkAttemptInvocationV1::Start(request) => {
                self.start(&request.identity, &request.lease, request.recovery)
                    .await?
            }
            WorkAttemptInvocationV1::PublishProgress(request) => {
                self.publish_progress(&request.identity, &request.lease, request.progress)
                    .await?
            }
            WorkAttemptInvocationV1::PublishArtifact(request) => {
                self.publish_artifact(&request.identity, &request.lease, request.artifact)
                    .await?
            }
            WorkAttemptInvocationV1::Cancel(request) => {
                self.cancel(&request.identity, &request.lease, request.request)
                    .await?
            }
            WorkAttemptInvocationV1::Recover(request) => {
                self.recover(&request.identity, &request.lease, request.reason)
                    .await?
            }
            WorkAttemptInvocationV1::Finish(request) => {
                self.finish(&request.identity, &request.lease, request.observed_at)
                    .await?
            }
            WorkAttemptInvocationV1::Terminalize(request) => {
                self.terminalize(&request.identity, &request.lease, request.terminal)
                    .await?
            }
        };
        Ok(attempt.into())
    }

    pub(crate) async fn acquire_lease(
        &self,
        snapshot: &WorkProjectionSnapshotV1,
        identity: WorkAttemptIdentityV1,
        execution: WorkExecutionEnvelopeV1,
        lease: WorkLeaseFenceV1,
    ) -> Result<WorkAttemptV1, WorkExecutionError> {
        let binding = self.binding(snapshot, identity.task_id())?;
        if execution.project_id() != self.authority.project_id()
            || execution.repository_id() != self.authority.repository_id()
            || execution.worktree_id() != self.authority.worktree_id()
            || execution.configuration_digest() != &self.configuration_digest
            || std::path::Path::new(execution.worktree_root()) != self.project_root
        {
            return Err(WorkProviderExecutionError::Rejected(
                "Work execution envelope does not match the registered authority".to_owned(),
            )
            .into());
        }
        let requested_route = execution.route().clone();
        let leased = self.execution.acquire_lease(
            &self.authority,
            WorkAttemptAcquireLeaseRequestV1 {
                snapshot: snapshot.clone(),
                identity,
                projection_binding: binding,
                execution,
                lease,
                requested_route,
            },
        )?;
        self.publish_activity("leased").await;
        Ok(leased)
    }

    /// Records the durable running intent, then admits it to the bounded queue.
    ///
    /// A refused admission leaves the durable intent in place and returns the
    /// bound that refused it: retrying `start` re-admits the same attempt and
    /// `recover` releases it. Nothing is compensated, so no provider effect can
    /// outlive a state the store never accepted.
    pub(crate) async fn start(
        &self,
        identity: &WorkAttemptIdentityV1,
        lease: &WorkLeaseFenceV1,
        recovery: WorkRecoveryStateV1,
    ) -> Result<WorkAttemptV1, WorkExecutionError> {
        let requested_route = self
            .attempt(identity)?
            .ok_or(WorkExecutionError::NotFound)?
            .requested_route()
            .clone();
        let running =
            self.execution
                .start(&self.authority, identity, lease, recovery, requested_route)?;
        // Re-check the published projection before the queue takes a slot. A
        // superseded proposal or replanned version must not consume capacity
        // under a lease that was exact when acquired and is exact no longer.
        let current = self
            .storage
            .projection(&self.authority, identity.task_id())
            .map_err(|error| match error {
                WorkStorageError::NotFoundOrNotAuthorized => WorkExecutionError::NotFound,
                WorkStorageError::Unavailable => WorkProviderExecutionError::Unavailable(
                    "Work projection authority is unavailable".to_owned(),
                )
                .into(),
                WorkStorageError::VersionConflict | WorkStorageError::IdempotencyConflict => {
                    WorkExecutionError::TerminalConflict
                }
            })?;
        running.validate_projection(&current)?;
        let queue = Arc::clone(&self.queue);
        let admitted = running.clone();
        tokio::task::spawn_blocking(move || queue.admit(&admitted))
            .await
            .map_err(|error| {
                WorkProviderExecutionError::Unavailable(format!(
                    "Codex Work admission task failed: {error}"
                ))
            })?
            .map_err(map_dispatch_error)?;
        self.publish_activity("running").await;
        Ok(running)
    }

    pub(crate) async fn publish_progress(
        &self,
        identity: &WorkAttemptIdentityV1,
        lease: &WorkLeaseFenceV1,
        progress: WorkAttemptProgressV1,
    ) -> Result<WorkAttemptV1, WorkExecutionError> {
        let attempt =
            self.execution
                .publish_progress(&self.authority, identity, lease, progress)?;
        self.publish_activity("progress").await;
        Ok(attempt)
    }

    pub(crate) async fn publish_artifact(
        &self,
        identity: &WorkAttemptIdentityV1,
        lease: &WorkLeaseFenceV1,
        artifact: WorkArtifactRefV1,
    ) -> Result<WorkAttemptV1, WorkExecutionError> {
        let attempt =
            self.execution
                .publish_artifact(&self.authority, identity, lease, artifact)?;
        self.publish_activity("artifact").await;
        Ok(attempt)
    }

    /// Claims the queue settlement for an attempt and acknowledges it durably.
    ///
    /// The durable cancellation intent, not the settlement variant, decides a
    /// cancelled outcome: a provider that finished before it observed the stop
    /// request must still terminate as cancelled, because that is the only
    /// terminal the recorded state admits. When this process holds no execution
    /// — after a restart, or after an earlier acknowledgement — the durable
    /// terminal is replayed and never re-derived.
    pub(crate) async fn finish(
        &self,
        identity: &WorkAttemptIdentityV1,
        lease: &WorkLeaseFenceV1,
        observed_at: UtcMicros,
    ) -> Result<WorkAttemptV1, WorkExecutionError> {
        let current = self.fenced_attempt(identity, lease)?;
        if current.is_terminal() {
            let _ = self.settle(identity, lease).await?;
            return Ok(current);
        }
        let terminal = if let WorkCancellationStateV1::Requested(request) = current.cancellation() {
            // A recorded cancellation admits exactly one terminal, so the
            // terminal is derived from durable state and draining the execution
            // is best-effort. Requiring a claimable settlement — or failing on
            // a drain error — would strand every attempt whose execution this
            // process no longer owns, and the transition table offers no other
            // way out of `CancellationRequested`.
            let _ = self.settle(identity, lease).await;
            self.execution.acknowledge_cancellation(
                &self.authority,
                identity,
                lease,
                WorkCancellationAcknowledgementV1::new(request.clone(), observed_at)?,
            )?;
            self.publish_activity("cancellation_acknowledged").await;
            WorkTerminalEvidenceV1::cancelled(
                cancelled_evidence_digest(identity, observed_at)?,
                observed_at,
            )?
        } else {
            // Deriving a terminal from what the provider actually did is the
            // only path that needs the settlement, so it is the only path that
            // may refuse without one.
            let Some(settlement) = self.settle(identity, lease).await? else {
                return Err(WorkProviderExecutionError::Unavailable(
                    "Codex Work execution is not owned by this process".to_owned(),
                )
                .into());
            };
            match settlement {
                WorkProviderSettlementV1::Completed { evidence } => {
                    let digest = canonical_sha256(&evidence).map_err(|error| {
                        WorkProviderExecutionError::Rejected(format!(
                            "Codex Work artifact digest failed: {error}"
                        ))
                    })?;
                    let artifact = WorkArtifactRefV1::new(
                        artifact_id(identity.attempt_id())?,
                        digest.clone(),
                        u64::try_from(evidence.len()).map_err(|_| {
                            WorkProviderExecutionError::Rejected(
                                "Codex Work artifact length overflowed".to_owned(),
                            )
                        })?,
                    )?;
                    self.publish_artifact(identity, lease, artifact).await?;
                    self.publish_progress(identity, lease, WorkAttemptProgressV1::new(1, 1)?)
                        .await?;
                    WorkTerminalEvidenceV1::succeeded(digest, observed_at)?
                }
                // A stop the store never requested cannot become a cancelled
                // terminal; the recorded state and the provider disagree.
                WorkProviderSettlementV1::Cancelled => {
                    return Err(WorkExecutionError::TerminalConflict);
                }
                WorkProviderSettlementV1::Failed { message } => WorkTerminalEvidenceV1::failed(
                    failed_evidence_digest(identity, &message, observed_at)?,
                    observed_at,
                )?,
                WorkProviderSettlementV1::TimedOut => WorkTerminalEvidenceV1::timed_out(
                    timed_out_evidence_digest(identity, observed_at)?,
                    observed_at,
                )?,
            }
        };
        let completed = self
            .execution
            .terminalize(&self.authority, identity, lease, terminal)?;
        self.publish_activity(attempt_state_key(completed.state()))
            .await;
        Ok(completed)
    }

    /// Records the durable cancellation intent, then stops the provider.
    pub(crate) async fn cancel(
        &self,
        identity: &WorkAttemptIdentityV1,
        lease: &WorkLeaseFenceV1,
        request: WorkCancellationRequestV1,
    ) -> Result<WorkAttemptV1, WorkExecutionError> {
        if let Some(current) = self.attempt(identity)?
            && current.is_terminal()
        {
            if current.lease() != lease {
                return Err(WorkExecutionError::StaleLease);
            }
            return match current.cancellation() {
                WorkCancellationStateV1::Acknowledged(acknowledgement)
                    if acknowledgement.request() == &request =>
                {
                    Ok(current)
                }
                _ => Err(WorkExecutionError::TerminalConflict),
            };
        }
        let acknowledged_at = request.requested_at();
        self.execution
            .request_cancellation(&self.authority, identity, lease, request)?;
        self.publish_activity("cancellation_requested").await;
        self.stop(identity, lease)?;
        self.finish(identity, lease, acknowledged_at).await
    }

    pub(crate) async fn recover(
        &self,
        identity: &WorkAttemptIdentityV1,
        lease: &WorkLeaseFenceV1,
        reason: WorkRestartReasonV1,
    ) -> Result<WorkAttemptV1, WorkExecutionError> {
        let attempt = self
            .execution
            .require_recovery(&self.authority, identity, lease, reason)?;
        self.stop(identity, lease)?;
        self.settle(identity, lease).await?;
        self.publish_activity("recovery_required").await;
        Ok(attempt)
    }

    pub(crate) fn renew_lease(
        &self,
        identity: &WorkAttemptIdentityV1,
        expected: &WorkLeaseFenceV1,
        replacement: WorkLeaseFenceV1,
    ) -> Result<WorkAttemptV1, WorkExecutionError> {
        self.execution
            .renew_lease(&self.authority, identity, expected, replacement)
    }

    pub(crate) fn attempt(
        &self,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<Option<WorkAttemptV1>, WorkExecutionError> {
        WorkAttemptPersistencePort::load(&self.storage, &self.authority, identity)
            .map_err(WorkExecutionError::Persistence)
    }

    /// Records a caller-supplied terminal and releases the execution it ends.
    ///
    /// Declaring an attempt terminal is the exposed completion path, so it must
    /// stop and join the provider: otherwise a completed attempt would hold its
    /// execution slot forever and the bound would become a standing refusal.
    pub(crate) async fn terminalize(
        &self,
        identity: &WorkAttemptIdentityV1,
        lease: &WorkLeaseFenceV1,
        terminal: WorkTerminalEvidenceV1,
    ) -> Result<WorkAttemptV1, WorkExecutionError> {
        let was_terminal = self
            .attempt(identity)?
            .is_some_and(|attempt| attempt.is_terminal());
        let completed = self
            .execution
            .terminalize(&self.authority, identity, lease, terminal)?;
        self.stop(identity, lease)?;
        self.settle(identity, lease).await?;
        if !was_terminal {
            self.publish_activity(attempt_state_key(completed.state()))
                .await;
        }
        Ok(completed)
    }

    fn fenced_attempt(
        &self,
        identity: &WorkAttemptIdentityV1,
        lease: &WorkLeaseFenceV1,
    ) -> Result<WorkAttemptV1, WorkExecutionError> {
        let attempt = self
            .attempt(identity)?
            .ok_or(WorkExecutionError::NotFound)?;
        if attempt.lease() != lease {
            return Err(WorkExecutionError::StaleLease);
        }
        Ok(attempt)
    }

    fn binding(
        &self,
        snapshot: &WorkProjectionSnapshotV1,
        task_id: &tracedecay_domain::TaskId,
    ) -> Result<WorkAttemptProjectionBindingV1, WorkExecutionError> {
        let generation_id = self.authority.projection_generation_id().map_err(|error| {
            WorkProviderExecutionError::Unavailable(format!(
                "work projection generation unavailable: {error}"
            ))
        })?;
        if snapshot.generation_id() != &generation_id {
            // Never copy a caller-forged snapshot generation into the binding.
            return Err(WorkExecutionError::NotFound);
        }
        let projection = snapshot
            .projections()
            .iter()
            .find(|projection| projection.task_id() == task_id)
            .ok_or(WorkExecutionError::NotFound)?;
        if projection.authority() != &self.authority {
            return Err(WorkExecutionError::NotFound);
        }
        if !projection.is_execution_admitted() {
            return Err(WorkExecutionError::NotFound);
        }
        let accepted_proposal = projection
            .accepted_proposal()
            .cloned()
            .ok_or(WorkExecutionError::NotFound)?;
        Ok(WorkAttemptProjectionBindingV1::new(
            generation_id,
            snapshot.sequence(),
            projection.version(),
            accepted_proposal,
        )?)
    }

    /// Asks the queue to stop an execution, tolerating one this process lost.
    fn stop(
        &self,
        identity: &WorkAttemptIdentityV1,
        lease: &WorkLeaseFenceV1,
    ) -> Result<(), WorkExecutionError> {
        match self.queue.cancel(identity, lease) {
            Ok(()) | Err(WorkDispatchError::Detached) => Ok(()),
            Err(error) => Err(map_dispatch_error(error)),
        }
    }

    /// Claims the settlement, reporting `None` when this process holds none.
    async fn settle(
        &self,
        identity: &WorkAttemptIdentityV1,
        lease: &WorkLeaseFenceV1,
    ) -> Result<Option<WorkProviderSettlementV1>, WorkExecutionError> {
        let queue = Arc::clone(&self.queue);
        let identity = identity.clone();
        let lease = lease.clone();
        let settled = tokio::task::spawn_blocking(move || queue.settle(&identity, &lease))
            .await
            .map_err(|error| {
                WorkProviderExecutionError::Unavailable(format!(
                    "Codex Work completion task failed: {error}"
                ))
            })?;
        match settled {
            Ok(settlement) => Ok(Some(settlement)),
            Err(WorkDispatchError::Detached) => Ok(None),
            Err(error) => Err(map_dispatch_error(error)),
        }
    }

    async fn publish_activity(&self, detail: &str) {
        event_lane::publish(
            self.observation_db.as_ref(),
            ActivityFamilyV1::Task,
            &self.project_root,
            Some(self.authority.project_id().as_str()),
            1,
            Some(detail),
        )
        .await;
    }
}

fn map_dispatch_error(error: WorkDispatchError) -> WorkExecutionError {
    match error {
        WorkDispatchError::Provider(provider) => WorkExecutionError::Provider(provider),
        other => {
            WorkExecutionError::Provider(WorkProviderExecutionError::Unavailable(other.to_string()))
        }
    }
}

const fn attempt_state_key(state: WorkAttemptStateV1) -> &'static str {
    match state {
        WorkAttemptStateV1::Leased => "leased",
        WorkAttemptStateV1::Running => "running",
        WorkAttemptStateV1::CancellationRequested => "cancellation_requested",
        WorkAttemptStateV1::CancellationAcknowledged => "cancellation_acknowledged",
        WorkAttemptStateV1::CancellationEscalated => "cancellation_escalated",
        WorkAttemptStateV1::RecoveryRequired => "recovery_required",
        WorkAttemptStateV1::Succeeded => "succeeded",
        WorkAttemptStateV1::Failed => "failed",
        WorkAttemptStateV1::TimedOut => "timed_out",
        WorkAttemptStateV1::Cancelled => "cancelled",
    }
}

fn cancelled_evidence_digest(
    identity: &WorkAttemptIdentityV1,
    observed_at: UtcMicros,
) -> Result<tracedecay_domain::ManifestDigest, WorkProviderExecutionError> {
    canonical_sha256(&("tracedecay.work.codex.cancelled.v1", identity, observed_at))
        .map_err(map_evidence_error)
}

fn failed_evidence_digest(
    identity: &WorkAttemptIdentityV1,
    message: &str,
    observed_at: UtcMicros,
) -> Result<tracedecay_domain::ManifestDigest, WorkProviderExecutionError> {
    canonical_sha256(&(
        "tracedecay.work.codex.failed.v1",
        identity,
        message,
        observed_at,
    ))
    .map_err(map_evidence_error)
}

fn timed_out_evidence_digest(
    identity: &WorkAttemptIdentityV1,
    observed_at: UtcMicros,
) -> Result<tracedecay_domain::ManifestDigest, WorkProviderExecutionError> {
    canonical_sha256(&(
        "tracedecay.work.provider.timed-out.v1",
        identity,
        observed_at,
    ))
    .map_err(map_evidence_error)
}

fn map_evidence_error(error: impl Display) -> WorkProviderExecutionError {
    WorkProviderExecutionError::Rejected(format!("Codex Work terminal evidence failed: {error}"))
}

fn artifact_id(attempt_id: &AttemptId) -> Result<WorkArtifactId, WorkProviderExecutionError> {
    WorkArtifactId::new(format!("artifact.work.codex.{}", attempt_id.as_str())).map_err(|error| {
        WorkProviderExecutionError::Rejected(format!(
            "canonical Codex artifact id is invalid: {error}"
        ))
    })
}
