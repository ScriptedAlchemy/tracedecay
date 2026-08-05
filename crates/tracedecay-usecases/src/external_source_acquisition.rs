//! Bounded canonical external-source acquisition.
//!
//! Event receipts are content-free wake-up evidence. The owner derives and
//! schedules only exact provider refreshes, while persistence, authorization,
//! network access, and canonical commit remain injected authorities.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;
use tracedecay_application::{
    SourceCanonicalRefetchAuthorityV1, SourceEventAdmissionContextV1, SourceEventAdmissionV1,
};
use tracedecay_domain::{
    ManifestDigest, SourceBindingIdentityV1, SourceBindingV1, SourceCoverageV1, SourceDefinitionV1,
    SourceEventAdmissionReceiptV1, SourceEventV1, SourceProviderEnvelopeV1, SourceRefreshCauseV1,
    SourceRefreshReceiptV1, SourceWholeRootStageV1, UtcMicros, canonical_sha256,
};
use tracedecay_store::SourceObjectMutationV1;

use crate::observation::ObservationCancellation;

const SOURCE_ACQUISITION_CAS_ATTEMPTS_V1: usize = 8;

pub type SourceAcquisitionFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceAcquisitionCasOutcomeV1 {
    Committed,
    Conflict,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("canonical external-source acquisition state is unavailable")]
pub struct SourceAcquisitionStateErrorV1;

pub trait SourceAcquisitionStatePortV1: Send + Sync {
    fn load<'a>(
        &'a self,
        binding: &'a SourceBindingIdentityV1,
    ) -> SourceAcquisitionFuture<
        'a,
        Result<Option<SourceAcquisitionQueueStateV1>, SourceAcquisitionStateErrorV1>,
    >;

    fn compare_and_swap<'a>(
        &'a self,
        binding: &'a SourceBindingIdentityV1,
        expected: Option<&'a ManifestDigest>,
        next: SourceAcquisitionQueueStateV1,
    ) -> SourceAcquisitionFuture<
        'a,
        Result<SourceAcquisitionCasOutcomeV1, SourceAcquisitionStateErrorV1>,
    >;

    fn next_ready<'a>(
        &'a self,
        now: UtcMicros,
    ) -> SourceAcquisitionFuture<
        'a,
        Result<Option<SourceAcquisitionQueueStateV1>, SourceAcquisitionStateErrorV1>,
    >;

    fn pending_count(
        &self,
    ) -> SourceAcquisitionFuture<'_, Result<usize, SourceAcquisitionStateErrorV1>>;
}

pub use tracedecay_store::{
    MAX_SOURCE_ACQUISITION_ATTEMPTS_V1, SourceAcquisitionQueueStateV1, SourceScheduledRefetchV1,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceAcquisitionGrantV1 {
    pub configuration_revision: u64,
    pub configuration_digest: ManifestDigest,
    pub sink_revision: u64,
    pub sink_digest: ManifestDigest,
    pub source_authorization_digest: ManifestDigest,
}

impl SourceAcquisitionGrantV1 {
    pub fn new(
        configuration_revision: u64,
        configuration_digest: ManifestDigest,
        sink_revision: u64,
        sink_digest: ManifestDigest,
        source_authorization_digest: ManifestDigest,
    ) -> Result<Self, ExternalSourceAcquisitionErrorV1> {
        let grant = Self {
            configuration_revision,
            configuration_digest,
            sink_revision,
            sink_digest,
            source_authorization_digest,
        };
        grant.validate()?;
        Ok(grant)
    }

    fn validate(&self) -> Result<(), ExternalSourceAcquisitionErrorV1> {
        if self.configuration_revision == 0 || self.sink_revision == 0 {
            return Err(ExternalSourceAcquisitionErrorV1::InvalidState);
        }
        self.configuration_digest
            .validate()
            .and_then(|_| self.sink_digest.validate())
            .and_then(|_| self.source_authorization_digest.validate())
            .map_err(|_| ExternalSourceAcquisitionErrorV1::InvalidState)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceAcquisitionAuthorizationPhaseV1 {
    BeforeFetch,
    BeforeCommit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceAcquisitionAuthorizationOutcomeV1 {
    Authorized(SourceAcquisitionGrantV1),
    Unauthorized,
    Unavailable,
}

pub trait SourceAcquisitionAuthorizationPortV1: Send + Sync {
    fn recheck<'a>(
        &'a self,
        task: &'a SourceScheduledRefetchV1,
        phase: SourceAcquisitionAuthorizationPhaseV1,
    ) -> SourceAcquisitionFuture<'a, SourceAcquisitionAuthorizationOutcomeV1>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceCanonicalRefetchPageV1 {
    pub envelope: SourceProviderEnvelopeV1,
    pub mutations: Vec<SourceObjectMutationV1>,
}

impl SourceCanonicalRefetchPageV1 {
    fn validate(
        &self,
        task: &SourceScheduledRefetchV1,
        grant: &SourceAcquisitionGrantV1,
    ) -> Result<(), ExternalSourceAcquisitionErrorV1> {
        task.validate()?;
        grant.validate()?;
        self.envelope
            .validate()
            .map_err(|_| ExternalSourceAcquisitionErrorV1::RemoteChange)?;
        let binding = task
            .binding()
            .immutable_identity()
            .map_err(|_| ExternalSourceAcquisitionErrorV1::InvalidState)?;
        if self.envelope.binding() != &binding
            || self.envelope.provider() != &task.definition().provider
            || self.envelope.refresh_id() != task.refresh().refresh_id()
            || self.envelope.cause() != SourceRefreshCauseV1::Event
            || self.envelope.capture_mode() != task.definition().capture_mode
            || self.envelope.refetch_strategy() != task.definition().refetch_strategy
            || self.envelope.coverage() == SourceCoverageV1::Unknown
            || matches!(
                self.envelope.kind(),
                tracedecay_domain::SourceEnvelopeKindV1::Unavailable
            )
            || self.mutations.len() > tracedecay_store::MAX_SOURCE_COMMIT_OBSERVATIONS_V1
        {
            return Err(ExternalSourceAcquisitionErrorV1::RemoteChange);
        }
        for mutation in &self.mutations {
            mutation
                .observation()
                .validate()
                .map_err(|_| ExternalSourceAcquisitionErrorV1::RemoteChange)?;
            mutation
                .evidence()
                .validate_against(&binding, self.envelope.partition(), mutation.observation())
                .map_err(|_| ExternalSourceAcquisitionErrorV1::RemoteChange)?;
            if mutation.evidence().source_authorization_digest()
                != &grant.source_authorization_digest
            {
                return Err(ExternalSourceAcquisitionErrorV1::RemoteChange);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceCanonicalRefetchOutcomeV1 {
    Fetched(SourceCanonicalRefetchPageV1),
    Unavailable,
    Retryable,
}

pub trait SourceCanonicalRefetchPortV1: Send + Sync {
    fn refetch<'a>(
        &'a self,
        task: &'a SourceScheduledRefetchV1,
        grant: &'a SourceAcquisitionGrantV1,
        cancellation: &'a ObservationCancellation,
    ) -> SourceAcquisitionFuture<'a, SourceCanonicalRefetchOutcomeV1>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceCanonicalCommitOutcomeV1 {
    Committed {
        coverage: SourceCoverageV1,
        whole_root_stage: Option<SourceWholeRootStageV1>,
    },
    ExactDuplicate {
        coverage: SourceCoverageV1,
        whole_root_stage: Option<SourceWholeRootStageV1>,
    },
    Unavailable,
}

pub trait SourceCanonicalCommitPortV1: Send + Sync {
    fn commit<'a>(
        &'a self,
        task: &'a SourceScheduledRefetchV1,
        grant: &'a SourceAcquisitionGrantV1,
        page: SourceCanonicalRefetchPageV1,
        authority: &'a SourceCanonicalRefetchAuthorityV1,
        cancellation: &'a ObservationCancellation,
    ) -> SourceAcquisitionFuture<'a, SourceCanonicalCommitOutcomeV1>;
}

#[derive(Clone, Debug)]
pub struct SourceAcquisitionPolicyV1 {
    max_attempts: u32,
    operation_budget: Duration,
    initial_backoff: Duration,
    maximum_backoff: Duration,
}

impl SourceAcquisitionPolicyV1 {
    pub fn new(
        max_attempts: u32,
        operation_budget: Duration,
        initial_backoff: Duration,
        maximum_backoff: Duration,
    ) -> Result<Self, ExternalSourceAcquisitionErrorV1> {
        if max_attempts == 0
            || max_attempts > MAX_SOURCE_ACQUISITION_ATTEMPTS_V1
            || operation_budget.is_zero()
            || initial_backoff.is_zero()
            || initial_backoff > maximum_backoff
        {
            return Err(ExternalSourceAcquisitionErrorV1::InvalidPolicy);
        }
        Ok(Self {
            max_attempts,
            operation_budget,
            initial_backoff,
            maximum_backoff,
        })
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ExternalSourceAcquisitionErrorV1 {
    #[error("external-source acquisition policy is invalid")]
    InvalidPolicy,
    #[error("external-source acquisition state is invalid")]
    InvalidState,
    #[error("external-source acquisition authority changed")]
    AuthorityChanged,
    #[error("external-source provider response changed its pinned refresh authority")]
    RemoteChange,
    #[error("external-source acquisition state is unavailable")]
    StateUnavailable,
    #[error("external-source acquisition state remained contended")]
    StateContended,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceAcquisitionRunOutcomeV1 {
    Idle,
    Committed {
        coverage: SourceCoverageV1,
        exact_duplicate: bool,
    },
    Partial {
        coverage: SourceCoverageV1,
        exact_duplicate: bool,
    },
    Unauthorized,
    Unavailable {
        attempt: u32,
        retry_at: UtcMicros,
    },
    Cancelled,
    Exhausted,
    BlockedRemoteChange,
}

pub struct ExternalSourceAcquisitionOwnerV1<S, A, F, C> {
    state: Arc<S>,
    authorization: Arc<A>,
    refetch: Arc<F>,
    commit: Arc<C>,
    policy: SourceAcquisitionPolicyV1,
    wake: tokio::sync::Notify,
}

impl<S, A, F, C> ExternalSourceAcquisitionOwnerV1<S, A, F, C>
where
    S: SourceAcquisitionStatePortV1,
    A: SourceAcquisitionAuthorizationPortV1,
    F: SourceCanonicalRefetchPortV1,
    C: SourceCanonicalCommitPortV1,
{
    pub fn new(
        state: Arc<S>,
        authorization: Arc<A>,
        refetch: Arc<F>,
        commit: Arc<C>,
        policy: SourceAcquisitionPolicyV1,
    ) -> Result<Self, ExternalSourceAcquisitionErrorV1> {
        Ok(Self {
            state,
            authorization,
            refetch,
            commit,
            policy,
            wake: tokio::sync::Notify::new(),
        })
    }

    pub async fn wait_for_wake(&self) {
        self.wake.notified().await;
    }

    pub async fn pending_count(&self) -> Result<usize, ExternalSourceAcquisitionErrorV1> {
        self.state
            .pending_count()
            .await
            .map_err(|_| ExternalSourceAcquisitionErrorV1::StateUnavailable)
    }

    pub async fn admit_event(
        &self,
        definition: &SourceDefinitionV1,
        binding: &SourceBindingV1,
        event: SourceEventV1,
        admitted_at: UtcMicros,
    ) -> Result<SourceEventAdmissionReceiptV1, ExternalSourceAcquisitionErrorV1> {
        definition
            .validate()
            .map_err(|_| ExternalSourceAcquisitionErrorV1::InvalidState)?;
        binding
            .validate_against(definition)
            .map_err(|_| ExternalSourceAcquisitionErrorV1::InvalidState)?;
        event
            .validate()
            .map_err(|_| ExternalSourceAcquisitionErrorV1::InvalidState)?;
        let identity = binding
            .immutable_identity()
            .map_err(|_| ExternalSourceAcquisitionErrorV1::InvalidState)?;
        if event.binding() != &identity {
            return Err(ExternalSourceAcquisitionErrorV1::AuthorityChanged);
        }
        for _ in 0..SOURCE_ACQUISITION_CAS_ATTEMPTS_V1 {
            let current = self
                .state
                .load(&identity)
                .await
                .map_err(|_| ExternalSourceAcquisitionErrorV1::StateUnavailable)?;
            if let Some(current) = &current {
                current
                    .validate()
                    .map_err(|_| ExternalSourceAcquisitionErrorV1::InvalidState)?;
                if current.definition() != definition || current.binding() != binding {
                    return Err(ExternalSourceAcquisitionErrorV1::AuthorityChanged);
                }
                if let Some(original) = current.receipt(event.event_key()) {
                    let duplicate = SourceEventAdmissionV1::admit(
                        definition,
                        binding,
                        event,
                        SourceEventAdmissionContextV1::Duplicate(original.clone()),
                    )
                    .map_err(|_| ExternalSourceAcquisitionErrorV1::InvalidState)?;
                    return Ok(duplicate.receipt().clone());
                }
            }
            let refresh = SourceRefreshReceiptV1::new(
                identity.clone(),
                definition.provider.clone(),
                canonical_sha256(&(
                    "tracedecay.external-source.event-refresh.v1",
                    &identity,
                    event.event_key(),
                    definition.revision,
                    &definition.definition_digest,
                    binding.binding_revision,
                    &binding.binding_digest,
                ))
                .map_err(|_| ExternalSourceAcquisitionErrorV1::InvalidState)?,
                SourceRefreshCauseV1::Event,
                definition.capture_mode,
                definition.refetch_strategy,
            )
            .map_err(|_| ExternalSourceAcquisitionErrorV1::InvalidState)?;
            let admission = if let Some(active) = current
                .as_ref()
                .and_then(SourceAcquisitionQueueStateV1::active)
            {
                SourceEventAdmissionV1::admit(
                    definition,
                    binding,
                    event.clone(),
                    SourceEventAdmissionContextV1::Coalesce(active.event_receipt().clone()),
                )
            } else {
                SourceEventAdmissionV1::admit(
                    definition,
                    binding,
                    event.clone(),
                    SourceEventAdmissionContextV1::Enqueue(refresh),
                )
            }
            .map_err(|_| ExternalSourceAcquisitionErrorV1::InvalidState)?;
            let mut receipts = current
                .as_ref()
                .map_or_else(BTreeMap::new, |state| state.receipts().clone());
            receipts.insert(
                admission.receipt().event_key().clone(),
                admission.receipt().clone(),
            );
            let scheduled = SourceScheduledRefetchV1::new(
                definition.clone(),
                binding.clone(),
                admission.receipt().clone(),
                None,
                0,
                admitted_at,
            )
            .map_err(|_| ExternalSourceAcquisitionErrorV1::InvalidState)?;
            let active = if admission.schedules_refresh() {
                Some(scheduled.clone())
            } else {
                current
                    .as_ref()
                    .and_then(SourceAcquisitionQueueStateV1::active)
                    .cloned()
            };
            let successor = if admission.schedules_refresh() {
                None
            } else {
                current
                    .as_ref()
                    .and_then(SourceAcquisitionQueueStateV1::successor)
                    .cloned()
                    .or(Some(scheduled))
            };
            let next = SourceAcquisitionQueueStateV1::new(
                definition.clone(),
                binding.clone(),
                active,
                successor,
                receipts,
            )
            .map_err(|_| ExternalSourceAcquisitionErrorV1::InvalidState)?;
            let expected = current
                .as_ref()
                .map(SourceAcquisitionQueueStateV1::state_digest);
            match self
                .state
                .compare_and_swap(&identity, expected, next)
                .await
                .map_err(|_| ExternalSourceAcquisitionErrorV1::StateUnavailable)?
            {
                SourceAcquisitionCasOutcomeV1::Committed => {
                    if admission.schedules_refresh() {
                        self.wake.notify_one();
                    }
                    return Ok(admission.receipt().clone());
                }
                SourceAcquisitionCasOutcomeV1::Conflict => {}
            }
        }
        Err(ExternalSourceAcquisitionErrorV1::StateContended)
    }

    pub async fn run_one(
        &self,
        now: UtcMicros,
        cancellation: &ObservationCancellation,
    ) -> Result<SourceAcquisitionRunOutcomeV1, ExternalSourceAcquisitionErrorV1> {
        if cancellation.is_cancelled() {
            return Ok(SourceAcquisitionRunOutcomeV1::Cancelled);
        }
        let Some(state) = self
            .state
            .next_ready(now)
            .await
            .map_err(|_| ExternalSourceAcquisitionErrorV1::StateUnavailable)?
        else {
            return Ok(SourceAcquisitionRunOutcomeV1::Idle);
        };
        state
            .validate()
            .map_err(|_| ExternalSourceAcquisitionErrorV1::InvalidState)?;
        let Some(task) = state.active().cloned() else {
            return Err(ExternalSourceAcquisitionErrorV1::InvalidState);
        };
        let event_admission = SourceEventAdmissionV1::resume(
            task.definition(),
            task.binding(),
            task.event_receipt().clone(),
        )
        .map_err(|_| ExternalSourceAcquisitionErrorV1::InvalidState)?;

        let grant = match self
            .bounded_authorization(&task, SourceAcquisitionAuthorizationPhaseV1::BeforeFetch)
            .await
        {
            SourceAcquisitionAuthorizationOutcomeV1::Authorized(grant) => {
                grant.validate()?;
                grant
            }
            SourceAcquisitionAuthorizationOutcomeV1::Unauthorized => {
                self.finish_state(&state, None).await?;
                return Ok(SourceAcquisitionRunOutcomeV1::Unauthorized);
            }
            SourceAcquisitionAuthorizationOutcomeV1::Unavailable => {
                return self.retry_or_exhaust(state, task, now).await;
            }
        };
        if cancellation.is_cancelled() {
            return Ok(SourceAcquisitionRunOutcomeV1::Cancelled);
        }
        let fetched = match tokio::time::timeout(
            self.policy.operation_budget,
            self.refetch.refetch(&task, &grant, cancellation),
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(_) => SourceCanonicalRefetchOutcomeV1::Unavailable,
        };
        let page = match fetched {
            SourceCanonicalRefetchOutcomeV1::Fetched(page) => page,
            SourceCanonicalRefetchOutcomeV1::Unavailable
            | SourceCanonicalRefetchOutcomeV1::Retryable => {
                return self.retry_or_exhaust(state, task, now).await;
            }
        };
        if page.validate(&task, &grant).is_err() {
            self.finish_state(&state, None).await?;
            return Ok(SourceAcquisitionRunOutcomeV1::BlockedRemoteChange);
        }
        if cancellation.is_cancelled() {
            return Ok(SourceAcquisitionRunOutcomeV1::Cancelled);
        }
        let commit_grant = match self
            .bounded_authorization(&task, SourceAcquisitionAuthorizationPhaseV1::BeforeCommit)
            .await
        {
            SourceAcquisitionAuthorizationOutcomeV1::Authorized(rechecked)
                if rechecked == grant =>
            {
                rechecked
            }
            SourceAcquisitionAuthorizationOutcomeV1::Authorized(_)
            | SourceAcquisitionAuthorizationOutcomeV1::Unauthorized => {
                self.finish_state(&state, None).await?;
                return Ok(SourceAcquisitionRunOutcomeV1::Unauthorized);
            }
            SourceAcquisitionAuthorizationOutcomeV1::Unavailable => {
                return self.retry_or_exhaust(state, task, now).await;
            }
        };
        if cancellation.is_cancelled() {
            return Ok(SourceAcquisitionRunOutcomeV1::Cancelled);
        }
        let commit = match tokio::time::timeout(
            self.policy.operation_budget,
            self.commit.commit(
                &task,
                &commit_grant,
                page,
                event_admission.canonical_refetch(),
                cancellation,
            ),
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(_) => SourceCanonicalCommitOutcomeV1::Unavailable,
        };
        let exact_duplicate = matches!(
            &commit,
            SourceCanonicalCommitOutcomeV1::ExactDuplicate { .. }
        );
        match commit {
            SourceCanonicalCommitOutcomeV1::Committed {
                coverage,
                whole_root_stage,
            }
            | SourceCanonicalCommitOutcomeV1::ExactDuplicate {
                coverage,
                whole_root_stage,
            } => {
                if coverage == SourceCoverageV1::Partial {
                    let continuation = task
                        .with_whole_root_stage(whole_root_stage, now)
                        .map_err(|_| ExternalSourceAcquisitionErrorV1::InvalidState)?;
                    self.finish_state(&state, Some(continuation)).await?;
                    self.wake.notify_one();
                    Ok(SourceAcquisitionRunOutcomeV1::Partial {
                        coverage,
                        exact_duplicate,
                    })
                } else {
                    self.finish_state(&state, None).await?;
                    Ok(SourceAcquisitionRunOutcomeV1::Committed {
                        coverage,
                        exact_duplicate,
                    })
                }
            }
            SourceCanonicalCommitOutcomeV1::Unavailable => {
                self.retry_or_exhaust(state, task, now).await
            }
        }
    }

    async fn bounded_authorization(
        &self,
        task: &SourceScheduledRefetchV1,
        phase: SourceAcquisitionAuthorizationPhaseV1,
    ) -> SourceAcquisitionAuthorizationOutcomeV1 {
        match tokio::time::timeout(
            self.policy.operation_budget,
            self.authorization.recheck(task, phase),
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(_) => SourceAcquisitionAuthorizationOutcomeV1::Unavailable,
        }
    }

    async fn retry_or_exhaust(
        &self,
        state: SourceAcquisitionQueueStateV1,
        task: SourceScheduledRefetchV1,
        now: UtcMicros,
    ) -> Result<SourceAcquisitionRunOutcomeV1, ExternalSourceAcquisitionErrorV1> {
        let next_attempt = task.attempt().saturating_add(1);
        if next_attempt >= self.policy.max_attempts {
            self.finish_state(&state, None).await?;
            return Ok(SourceAcquisitionRunOutcomeV1::Exhausted);
        }
        let retry_at = add_duration(now, self.policy.backoff(task.attempt()))?;
        let task = task
            .with_retry(next_attempt, retry_at)
            .map_err(|_| ExternalSourceAcquisitionErrorV1::InvalidState)?;
        self.finish_state(&state, Some(task)).await?;
        Ok(SourceAcquisitionRunOutcomeV1::Unavailable {
            attempt: next_attempt,
            retry_at,
        })
    }

    async fn finish_state(
        &self,
        current: &SourceAcquisitionQueueStateV1,
        active: Option<SourceScheduledRefetchV1>,
    ) -> Result<(), ExternalSourceAcquisitionErrorV1> {
        let binding = current
            .binding_identity()
            .map_err(|_| ExternalSourceAcquisitionErrorV1::InvalidState)?;
        let (active, successor) = match active {
            Some(active) => (Some(active), current.successor().cloned()),
            None => (current.successor().cloned(), None),
        };
        let next = current
            .with_schedule(active, successor)
            .map_err(|_| ExternalSourceAcquisitionErrorV1::InvalidState)?;
        match self
            .state
            .compare_and_swap(&binding, Some(current.state_digest()), next)
            .await
            .map_err(|_| ExternalSourceAcquisitionErrorV1::StateUnavailable)?
        {
            SourceAcquisitionCasOutcomeV1::Committed => Ok(()),
            SourceAcquisitionCasOutcomeV1::Conflict => {
                Err(ExternalSourceAcquisitionErrorV1::StateContended)
            }
        }
    }
}

impl SourceAcquisitionPolicyV1 {
    fn backoff(&self, attempt: u32) -> Duration {
        let factor = 1_u32 << attempt;
        self.initial_backoff
            .saturating_mul(factor)
            .min(self.maximum_backoff)
    }
}

fn add_duration(
    timestamp: UtcMicros,
    duration: Duration,
) -> Result<UtcMicros, ExternalSourceAcquisitionErrorV1> {
    let micros = i64::try_from(duration.as_micros())
        .map_err(|_| ExternalSourceAcquisitionErrorV1::InvalidPolicy)?;
    timestamp
        .0
        .checked_add(micros)
        .map(UtcMicros)
        .ok_or(ExternalSourceAcquisitionErrorV1::InvalidState)
}

#[path = "external_source_acquisition_store.rs"]
mod canonical_store;

#[cfg(test)]
#[path = "external_source_acquisition_tests.rs"]
mod tests;
