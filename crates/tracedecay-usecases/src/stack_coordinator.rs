//! Centralized branch-stack transition, optional-preflight, and fanout owner.
//!
//! GitHub/CI/native adapters supply observations only. This coordinator owns
//! comparison, deterministic delivery, authorization rechecks, bounded
//! optional preflight, dedupe, and the scoped circuit breaker. It never calls
//! native integration apply or a provider write.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Condvar, Mutex};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_application::{
    CancellationSignal, NativeIntegrationPreflightOutcomeV1, NativeIntegrationPreflightRequestV1,
};
use tracedecay_domain::{
    ActorId, BranchStackRevisionId, GitHubStackSnapshotV1, ManifestDigest, ProjectId, RepositoryId,
    StackDeliveryWatermarkId, StackSignalId, UtcMicros, canonical_sha256,
};

pub const MAX_REPOSITORY_PREFLIGHTS: usize = 4;
pub const MAX_DAEMON_PREFLIGHTS: usize = 16;
pub const MAX_BATCH_RECIPIENTS: usize = 64;
pub const MAX_BATCH_SIGNALS: usize = 128;
pub const FAST_DEBOUNCE_MICROS: i64 = 250_000;
pub const DRIFT_DEBOUNCE_MICROS: i64 = 1_000_000;
pub const DEDUPE_TTL_MICROS: i64 = 300_000_000;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum StackSignalKindV1 {
    DependencyReady,
    ActualConflict,
    PotentialConflict,
    StackTipDrift,
    PullRequestDrift,
    CiEvaluatedCommitDrift,
    IntegrationCommitted,
    IntegrationNeedsInspection,
    AuthorizationLost,
}

impl StackSignalKindV1 {
    const fn debounce_micros(self) -> i64 {
        match self {
            Self::DependencyReady | Self::PotentialConflict => FAST_DEBOUNCE_MICROS,
            Self::StackTipDrift | Self::PullRequestDrift | Self::CiEvaluatedCommitDrift => {
                DRIFT_DEBOUNCE_MICROS
            }
            Self::ActualConflict
            | Self::IntegrationCommitted
            | Self::IntegrationNeedsInspection
            | Self::AuthorizationLost => 0,
        }
    }

    pub const fn is_material(self) -> bool {
        matches!(
            self,
            Self::ActualConflict
                | Self::IntegrationCommitted
                | Self::IntegrationNeedsInspection
                | Self::AuthorizationLost
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StackSignalV1 {
    pub signal_id: StackSignalId,
    pub project_id: ProjectId,
    pub repository_id: RepositoryId,
    pub stack_revision_id: BranchStackRevisionId,
    pub stack_revision_digest: ManifestDigest,
    pub kind: StackSignalKindV1,
    pub state_digest: ManifestDigest,
    pub github_stack_digest: Option<ManifestDigest>,
    pub observed_at: UtcMicros,
    pub watermark_id: StackDeliveryWatermarkId,
}

impl StackSignalV1 {
    pub fn validate(&self) -> Result<(), StackCoordinatorError> {
        self.signal_id
            .validate()
            .map_err(|error| StackCoordinatorError::Invalid(error.to_string()))?;
        self.project_id
            .validate()
            .map_err(|error| StackCoordinatorError::Invalid(error.to_string()))?;
        self.repository_id
            .validate()
            .map_err(|error| StackCoordinatorError::Invalid(error.to_string()))?;
        self.stack_revision_id
            .validate()
            .map_err(|error| StackCoordinatorError::Invalid(error.to_string()))?;
        self.stack_revision_digest
            .validate()
            .map_err(|error| StackCoordinatorError::Invalid(error.to_string()))?;
        self.state_digest
            .validate()
            .map_err(|error| StackCoordinatorError::Invalid(error.to_string()))?;
        self.github_stack_digest
            .as_ref()
            .map_or(Ok(()), ManifestDigest::validate)
            .map_err(|error| StackCoordinatorError::Invalid(error.to_string()))?;
        self.watermark_id
            .validate()
            .map_err(|error| StackCoordinatorError::Invalid(error.to_string()))
    }

    fn due_at(&self) -> UtcMicros {
        UtcMicros(
            self.observed_at
                .0
                .saturating_add(self.kind.debounce_micros()),
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct StackPendingDeliveryV1 {
    pub recipient: ActorId,
    pub signal_id: StackSignalId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StackDeliveryBatchV1 {
    pub watermark_id: StackDeliveryWatermarkId,
    pub recipients: Vec<ActorId>,
    pub signals: Vec<StackSignalV1>,
    pub deliveries: Vec<StackPendingDeliveryV1>,
    pub partial: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StackDeliveryAuthorizationV1 {
    Authorized,
    Denied,
    Stale,
    Unavailable,
}

pub trait StackCoordinatorStore: Send + Sync {
    fn append_signal(
        &self,
        signal: StackSignalV1,
        recipients: Vec<ActorId>,
    ) -> Result<(), StackCoordinatorError>;

    fn pending_deliveries(
        &self,
    ) -> Result<Vec<(StackPendingDeliveryV1, StackSignalV1)>, StackCoordinatorError>;

    fn acknowledge(
        &self,
        watermark_id: &StackDeliveryWatermarkId,
        deliveries: &[StackPendingDeliveryV1],
    ) -> Result<(), StackCoordinatorError>;

    fn signal(
        &self,
        signal_id: &StackSignalId,
    ) -> Result<Option<StackSignalV1>, StackCoordinatorError>;
}

pub trait StackDeliveryAuthorizationPort: Send + Sync {
    fn authorize(
        &self,
        recipient: &ActorId,
        signal: &StackSignalV1,
    ) -> StackDeliveryAuthorizationV1;
}

pub trait StackDeliveryPort: Send + Sync {
    fn deliver(&self, batch: &StackDeliveryBatchV1) -> Result<(), StackCoordinatorError>;
}

pub trait OptionalStackPreflightPort: Send + Sync {
    fn preflight(
        &self,
        request: &NativeIntegrationPreflightRequestV1,
        cancellation: &CancellationSignal,
    ) -> Result<NativeIntegrationPreflightOutcomeV1, StackCoordinatorError>;
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct CircuitKey {
    repository_id: RepositoryId,
    scope_digest: ManifestDigest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CircuitState {
    Closed,
    Open { until: UtcMicros },
    HalfOpenProbe,
}

#[derive(Clone, Debug)]
struct Circuit {
    state: CircuitState,
    failures: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StackCircuitPolicyV1 {
    pub revision: u64,
    pub policy_digest: ManifestDigest,
    pub failure_threshold: u32,
    pub open_micros: i64,
}

impl StackCircuitPolicyV1 {
    pub fn seal(mut self) -> Result<Self, StackCoordinatorError> {
        if self.revision == 0 || self.failure_threshold == 0 || self.open_micros <= 0 {
            return Err(StackCoordinatorError::Invalid(
                "invalid stack circuit policy".to_owned(),
            ));
        }
        self.policy_digest = self.compute_digest()?;
        Ok(self)
    }

    fn compute_digest(&self) -> Result<ManifestDigest, StackCoordinatorError> {
        canonical_sha256(&(
            "tracedecay.stack-circuit-policy.v1",
            self.revision,
            self.failure_threshold,
            self.open_micros,
        ))
        .map_err(|error| StackCoordinatorError::Invalid(error.to_string()))
    }

    pub fn validate(&self) -> Result<(), StackCoordinatorError> {
        self.policy_digest
            .validate()
            .map_err(|error| StackCoordinatorError::Invalid(error.to_string()))?;
        if self.revision == 0 || self.failure_threshold == 0 || self.open_micros <= 0 {
            return Err(StackCoordinatorError::Invalid(
                "invalid stack circuit policy".to_owned(),
            ));
        }
        if self.compute_digest()? != self.policy_digest {
            return Err(StackCoordinatorError::Invalid(
                "stack circuit policy digest mismatch".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptionalPreflightDispositionV1 {
    Complete,
    Partial,
    SuppressedOpenCircuit,
    Saturated,
    Cancelled,
    Stale,
    Denied,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct PreflightKey {
    repository_id: RepositoryId,
    scope_digest: ManifestDigest,
    stack_revision_digest: ManifestDigest,
    request_digest: ManifestDigest,
}

struct InFlight {
    result: Mutex<Option<Result<OptionalPreflightDispositionV1, StackCoordinatorError>>>,
    complete: Condvar,
}

#[derive(Default)]
struct PreflightState {
    daemon_active: usize,
    repository_active: BTreeMap<RepositoryId, usize>,
    in_flight: BTreeMap<PreflightKey, Arc<InFlight>>,
}

pub struct StackCoordinator<S, A, D, P> {
    store: Arc<S>,
    authorization: Arc<A>,
    delivery: Arc<D>,
    preflight: Arc<P>,
    policy: StackCircuitPolicyV1,
    dedupe:
        Mutex<BTreeMap<(ProjectId, RepositoryId, StackSignalKindV1, ManifestDigest), UtcMicros>>,
    circuits: Mutex<BTreeMap<CircuitKey, Circuit>>,
    preflights: Mutex<PreflightState>,
}

impl<S, A, D, P> StackCoordinator<S, A, D, P>
where
    S: StackCoordinatorStore,
    A: StackDeliveryAuthorizationPort,
    D: StackDeliveryPort,
    P: OptionalStackPreflightPort,
{
    pub fn new(
        store: Arc<S>,
        authorization: Arc<A>,
        delivery: Arc<D>,
        preflight: Arc<P>,
        policy: StackCircuitPolicyV1,
    ) -> Result<Self, StackCoordinatorError> {
        policy.validate()?;
        Ok(Self {
            store,
            authorization,
            delivery,
            preflight,
            policy,
            dedupe: Mutex::new(BTreeMap::new()),
            circuits: Mutex::new(BTreeMap::new()),
            preflights: Mutex::new(PreflightState::default()),
        })
    }

    pub fn enqueue(
        &self,
        signal: StackSignalV1,
        mut recipients: Vec<ActorId>,
    ) -> Result<(), StackCoordinatorError> {
        signal.validate()?;
        recipients.sort();
        recipients.dedup();
        for recipient in &recipients {
            recipient
                .validate()
                .map_err(|error| StackCoordinatorError::Invalid(error.to_string()))?;
        }
        let mut authorized = Vec::with_capacity(recipients.len());
        for recipient in recipients {
            match self.authorization.authorize(&recipient, &signal) {
                StackDeliveryAuthorizationV1::Authorized => authorized.push(recipient),
                StackDeliveryAuthorizationV1::Denied | StackDeliveryAuthorizationV1::Stale => {}
                StackDeliveryAuthorizationV1::Unavailable => {
                    return Err(StackCoordinatorError::Unavailable);
                }
            }
        }
        if !signal.kind.is_material() {
            let key = (
                signal.project_id.clone(),
                signal.repository_id.clone(),
                signal.kind,
                signal.state_digest.clone(),
            );
            let mut dedupe = self.dedupe.lock().map_err(lock_error)?;
            dedupe.retain(|_, observed_at| {
                signal.observed_at.0.saturating_sub(observed_at.0) <= DEDUPE_TTL_MICROS
            });
            if dedupe.get(&key).is_some_and(|observed_at| {
                signal.observed_at.0.saturating_sub(observed_at.0) <= DEDUPE_TTL_MICROS
            }) {
                return Ok(());
            }
            self.store.append_signal(signal.clone(), authorized)?;
            dedupe.insert(key, signal.observed_at);
            return Ok(());
        }
        self.store.append_signal(signal, authorized)
    }

    /// Deterministically drains every due transition. Overflow becomes later
    /// batches and is never dropped.
    pub fn drain_due(&self, now: UtcMicros) -> Result<usize, StackCoordinatorError> {
        let mut pending = self.store.pending_deliveries()?;
        pending.retain(|(_, signal)| signal.kind.is_material() || signal.due_at().0 <= now.0);
        pending.sort_by(
            |(left_delivery, left_signal), (right_delivery, right_signal)| {
                (
                    left_signal.observed_at,
                    &left_signal.signal_id,
                    &left_delivery.recipient,
                )
                    .cmp(&(
                        right_signal.observed_at,
                        &right_signal.signal_id,
                        &right_delivery.recipient,
                    ))
            },
        );
        let mut delivered = 0;
        while !pending.is_empty() {
            let mut recipients = BTreeSet::new();
            let mut signal_ids = BTreeSet::new();
            let mut deliveries = Vec::new();
            let mut acknowledged = Vec::new();
            let mut signals = BTreeMap::new();
            let mut consumed = 0;
            for (delivery, signal) in &pending {
                if signal.watermark_id != pending[0].1.watermark_id {
                    break;
                }
                if (recipients.len() >= MAX_BATCH_RECIPIENTS
                    && !recipients.contains(&delivery.recipient))
                    || (signal_ids.len() >= MAX_BATCH_SIGNALS
                        && !signal_ids.contains(&delivery.signal_id))
                {
                    break;
                }
                consumed += 1;
                match self.authorization.authorize(&delivery.recipient, signal) {
                    StackDeliveryAuthorizationV1::Authorized => {
                        recipients.insert(delivery.recipient.clone());
                        signal_ids.insert(delivery.signal_id.clone());
                        deliveries.push(delivery.clone());
                        acknowledged.push(delivery.clone());
                        signals
                            .entry(signal.signal_id.clone())
                            .or_insert_with(|| signal.clone());
                    }
                    StackDeliveryAuthorizationV1::Denied | StackDeliveryAuthorizationV1::Stale => {
                        acknowledged.push(delivery.clone());
                    }
                    StackDeliveryAuthorizationV1::Unavailable => {}
                }
            }
            if consumed == 0 {
                return Err(StackCoordinatorError::Invalid(
                    "fanout batch made no progress".to_owned(),
                ));
            }
            let watermark_id = pending[0].1.watermark_id.clone();
            let deliverable = StackDeliveryBatchV1 {
                watermark_id: watermark_id.clone(),
                recipients: recipients.into_iter().collect(),
                signals: signals.into_values().collect(),
                deliveries: deliveries.clone(),
                partial: consumed < pending.len(),
            };
            if !deliverable.recipients.is_empty() {
                self.delivery.deliver(&deliverable)?;
                delivered += deliverable.deliveries.len();
            }
            if !acknowledged.is_empty() {
                self.store.acknowledge(&watermark_id, &acknowledged)?;
            }
            pending.drain(..consumed);
        }
        Ok(delivered)
    }

    pub fn expand(
        &self,
        recipient: &ActorId,
        signal_id: &StackSignalId,
    ) -> Result<Option<StackSignalV1>, StackCoordinatorError> {
        let Some(signal) = self.store.signal(signal_id)? else {
            return Ok(None);
        };
        Ok((self.authorization.authorize(recipient, &signal)
            == StackDeliveryAuthorizationV1::Authorized)
            .then_some(signal))
    }

    /// Optional preflight fanout. Identical requests join the same in-flight
    /// operation; counters enforce four per repository and sixteen per daemon.
    pub fn optional_preflight(
        &self,
        request: &NativeIntegrationPreflightRequestV1,
        cancellation: &CancellationSignal,
        now: UtcMicros,
    ) -> Result<OptionalPreflightDispositionV1, StackCoordinatorError> {
        let request_digest = canonical_sha256(request)
            .map_err(|error| StackCoordinatorError::Invalid(error.to_string()))?;
        let key = PreflightKey {
            repository_id: request.topology.destination.repository_id.clone(),
            scope_digest: request.topology.destination.scope_digest.clone(),
            stack_revision_digest: match &request.topology.selection {
                tracedecay_application::NativeIntegrationSelectionBindingV1::DeclaredStackEdge {
                    revision_digest,
                    ..
                } => revision_digest.clone(),
                tracedecay_application::NativeIntegrationSelectionBindingV1::IndependentBranch {
                    proposal_digest,
                } => proposal_digest.clone(),
            },
            request_digest,
        };
        let circuit_key = CircuitKey {
            repository_id: key.repository_id.clone(),
            scope_digest: key.scope_digest.clone(),
        };
        if !self.circuit_admit(&circuit_key, now)? {
            return Ok(OptionalPreflightDispositionV1::SuppressedOpenCircuit);
        }
        let (flight, owner) = {
            let mut state = self.preflights.lock().map_err(lock_error)?;
            if let Some(flight) = state.in_flight.get(&key) {
                (flight.clone(), false)
            } else {
                let repository_active = state
                    .repository_active
                    .get(&key.repository_id)
                    .copied()
                    .unwrap_or(0);
                if repository_active >= MAX_REPOSITORY_PREFLIGHTS
                    || state.daemon_active >= MAX_DAEMON_PREFLIGHTS
                {
                    drop(state);
                    self.record_circuit_failure(&circuit_key, now)?;
                    return Ok(OptionalPreflightDispositionV1::Saturated);
                }
                state.daemon_active += 1;
                *state
                    .repository_active
                    .entry(key.repository_id.clone())
                    .or_default() += 1;
                let flight = Arc::new(InFlight {
                    result: Mutex::new(None),
                    complete: Condvar::new(),
                });
                state.in_flight.insert(key.clone(), flight.clone());
                (flight, true)
            }
        };
        if !owner {
            let mut result = flight.result.lock().map_err(lock_error)?;
            while result.is_none() {
                result = flight.complete.wait(result).map_err(lock_error)?;
            }
            return result.as_ref().cloned().ok_or_else(|| {
                StackCoordinatorError::Invalid("missing joined result".to_owned())
            })?;
        }
        let result = self
            .preflight
            .preflight(request, cancellation)
            .map(classify_preflight);
        {
            let mut stored = flight.result.lock().map_err(lock_error)?;
            *stored = Some(result.clone());
            flight.complete.notify_all();
        }
        {
            let mut state = self.preflights.lock().map_err(lock_error)?;
            state.in_flight.remove(&key);
            state.daemon_active = state.daemon_active.saturating_sub(1);
            if let Some(active) = state.repository_active.get_mut(&key.repository_id) {
                *active = active.saturating_sub(1);
                if *active == 0 {
                    state.repository_active.remove(&key.repository_id);
                }
            }
        }
        match &result {
            Ok(OptionalPreflightDispositionV1::Complete) => {
                self.record_circuit_success(&circuit_key)?;
            }
            Ok(
                OptionalPreflightDispositionV1::Partial
                | OptionalPreflightDispositionV1::Cancelled
                | OptionalPreflightDispositionV1::Unavailable,
            )
            | Err(_) => self.record_circuit_failure(&circuit_key, now)?,
            Ok(
                OptionalPreflightDispositionV1::Stale
                | OptionalPreflightDispositionV1::Denied
                | OptionalPreflightDispositionV1::Saturated,
            ) => self.record_circuit_non_closing(&circuit_key, now)?,
            Ok(OptionalPreflightDispositionV1::SuppressedOpenCircuit) => {}
        }
        result
    }

    fn circuit_admit(
        &self,
        key: &CircuitKey,
        now: UtcMicros,
    ) -> Result<bool, StackCoordinatorError> {
        let mut circuits = self.circuits.lock().map_err(lock_error)?;
        let circuit = circuits.entry(key.clone()).or_insert(Circuit {
            state: CircuitState::Closed,
            failures: 0,
        });
        match circuit.state {
            CircuitState::Closed => Ok(true),
            CircuitState::Open { until } if now.0 >= until.0 => {
                circuit.state = CircuitState::HalfOpenProbe;
                Ok(true)
            }
            CircuitState::Open { .. } | CircuitState::HalfOpenProbe => Ok(false),
        }
    }

    fn record_circuit_success(&self, key: &CircuitKey) -> Result<(), StackCoordinatorError> {
        let mut circuits = self.circuits.lock().map_err(lock_error)?;
        circuits.insert(
            key.clone(),
            Circuit {
                state: CircuitState::Closed,
                failures: 0,
            },
        );
        Ok(())
    }

    fn record_circuit_failure(
        &self,
        key: &CircuitKey,
        now: UtcMicros,
    ) -> Result<(), StackCoordinatorError> {
        let mut circuits = self.circuits.lock().map_err(lock_error)?;
        let circuit = circuits.entry(key.clone()).or_insert(Circuit {
            state: CircuitState::Closed,
            failures: 0,
        });
        circuit.failures = circuit.failures.saturating_add(1);
        if circuit.failures >= self.policy.failure_threshold
            || circuit.state == CircuitState::HalfOpenProbe
        {
            circuit.state = CircuitState::Open {
                until: UtcMicros(now.0.saturating_add(self.policy.open_micros)),
            };
        }
        Ok(())
    }

    fn record_circuit_non_closing(
        &self,
        key: &CircuitKey,
        now: UtcMicros,
    ) -> Result<(), StackCoordinatorError> {
        let mut circuits = self.circuits.lock().map_err(lock_error)?;
        if let Some(circuit) = circuits.get_mut(key)
            && circuit.state == CircuitState::HalfOpenProbe
        {
            circuit.state = CircuitState::Open {
                until: UtcMicros(now.0.saturating_add(self.policy.open_micros)),
            };
        }
        Ok(())
    }
}

/// Exact read-only GitHub stacked-PR observation. Broken/partial topology is
/// represented by the capability owner and never repaired here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitHubStackTransitionV1 {
    pub previous: Option<GitHubStackSnapshotV1>,
    pub current: GitHubStackSnapshotV1,
}

impl GitHubStackTransitionV1 {
    pub fn state_digest(&self) -> Result<ManifestDigest, StackCoordinatorError> {
        self.current
            .validate()
            .map_err(|error| StackCoordinatorError::Invalid(error.to_string()))?;
        if let Some(previous) = &self.previous {
            previous
                .validate()
                .map_err(|error| StackCoordinatorError::Invalid(error.to_string()))?;
            if previous.capability.project_id != self.current.capability.project_id
                || previous.capability.repository_id != self.current.capability.repository_id
            {
                return Err(StackCoordinatorError::Invalid(
                    "GitHub stack transition crossed repository authority".to_owned(),
                ));
            }
        }
        canonical_sha256(&(
            "tracedecay.github-stack.transition.v1",
            self.previous
                .as_ref()
                .map(|snapshot| &snapshot.content_digest),
            &self.current.content_digest,
        ))
        .map_err(|error| StackCoordinatorError::Invalid(error.to_string()))
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum StackCoordinatorError {
    #[error("stack coordinator is unavailable")]
    Unavailable,
    #[error("stack coordinator authorization was denied")]
    Denied,
    #[error("stack coordinator state is stale")]
    Stale,
    #[error("stack coordinator operation was cancelled")]
    Cancelled,
    #[error("stack coordinator contract is invalid: {0}")]
    Invalid(String),
}

fn classify_preflight(
    outcome: NativeIntegrationPreflightOutcomeV1,
) -> OptionalPreflightDispositionV1 {
    match outcome {
        NativeIntegrationPreflightOutcomeV1::Preview(_) => OptionalPreflightDispositionV1::Complete,
        NativeIntegrationPreflightOutcomeV1::Partial
        | NativeIntegrationPreflightOutcomeV1::DurabilityUncertain
        | NativeIntegrationPreflightOutcomeV1::ResetRequired => {
            OptionalPreflightDispositionV1::Partial
        }
        NativeIntegrationPreflightOutcomeV1::Stale => OptionalPreflightDispositionV1::Stale,
        NativeIntegrationPreflightOutcomeV1::Denied => OptionalPreflightDispositionV1::Denied,
        NativeIntegrationPreflightOutcomeV1::Unavailable => {
            OptionalPreflightDispositionV1::Unavailable
        }
        NativeIntegrationPreflightOutcomeV1::Cancelled => OptionalPreflightDispositionV1::Cancelled,
    }
}

fn lock_error<T>(error: std::sync::PoisonError<T>) -> StackCoordinatorError {
    StackCoordinatorError::Invalid(format!("stack coordinator lock poisoned: {error}"))
}

#[cfg(test)]
#[path = "stack_coordinator_tests.rs"]
mod tests;
