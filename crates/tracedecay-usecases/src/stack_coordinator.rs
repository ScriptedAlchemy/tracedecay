use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracedecay_application::{
    CancellationSignal, NativeIntegrationPreflightOutcomeV1, NativeIntegrationPreflightRequestV1,
    ResolvedScope,
};
use tracedecay_domain::configuration::GitHubStackedPullRequestPolicyV1;
use tracedecay_domain::{
    ActorId, AnchorOwnerBindingV1, CommitId, GitHubStackCapabilitySnapshotV1,
    GitHubStackCapabilityStateV1, GitHubStackLayerSnapshotV1, GitHubStackSnapshotV1,
    GitTopologyAnchorTargetV1, ManifestDigest, ObservationScopeV1, PrivacyDomainBoundLocatorDigest,
    ProjectionGenerationId, ProviderId, PullRequestSnapshotAnchorRefV1, RefId, RepositoryId,
    RetrievalAnchorId, StackSignalId, UtcMicros, WorkStackDriftObservedV1, canonical_sha256,
    derive_git_topology_anchor_id,
};

mod transition;
pub use transition::*;

pub const MAX_GITHUB_STACK_ROOTS_PER_FANOUT_V1: usize = 64;
pub const MAX_GITHUB_STACK_SIGNALS_PER_FANOUT_V1: usize = 128;
pub const MAX_GITHUB_STACK_LAYERS_V1: usize = 100;
const PREFLIGHT_JOIN_WAIT_SLICE: Duration = Duration::from_millis(25);
const PREFLIGHT_JOIN_TIMEOUT: Duration = Duration::from_secs(5);

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

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct PreflightKey {
    repository_id: RepositoryId,
    scope_digest: ManifestDigest,
    revision_digest: ManifestDigest,
    request_digest: ManifestDigest,
}

struct InFlight {
    result: Mutex<Option<Result<OptionalPreflightDispositionV1, StackCoordinatorErrorV1>>>,
    complete: Condvar,
}

#[derive(Default)]
struct PreflightState {
    daemon_active: usize,
    repository_active: BTreeMap<RepositoryId, usize>,
    in_flight: BTreeMap<PreflightKey, Arc<InFlight>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitHubStackProviderLayerV1 {
    pub provider_position: u32,
    pub pull_request: PullRequestSnapshotAnchorRefV1,
    pub base_ref_id: RefId,
    pub head_ref_id: RefId,
    pub protection_digest: ManifestDigest,
    pub ci_digest: ManifestDigest,
    pub merge_queue_digest: ManifestDigest,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitHubStackProviderSnapshotV1 {
    pub response_digest: ManifestDigest,
    pub provider_stack_id_digest: PrivacyDomainBoundLocatorDigest,
    pub final_target_ref_id: RefId,
    pub final_target_commit_id: CommitId,
    pub layers: Vec<GitHubStackProviderLayerV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitHubStackProviderSourceBindingV1 {
    pub owner: AnchorOwnerBindingV1,
    pub capability_source_anchor_id: RetrievalAnchorId,
    pub snapshot_source_anchor_id: Option<RetrievalAnchorId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitHubStackProviderOutcomeV1 {
    Unavailable,
    EnabledWithoutStack { response_digest: ManifestDigest },
    Enabled(GitHubStackProviderSnapshotV1),
    Degraded { response_digest: ManifestDigest },
}

impl GitHubStackProviderOutcomeV1 {
    fn response_digest(&self) -> Option<&ManifestDigest> {
        match self {
            Self::Unavailable => None,
            Self::EnabledWithoutStack { response_digest } | Self::Degraded { response_digest } => {
                Some(response_digest)
            }
            Self::Enabled(snapshot) => Some(&snapshot.response_digest),
        }
    }

    const fn capability_state(&self) -> GitHubStackCapabilityStateV1 {
        match self {
            Self::Unavailable => GitHubStackCapabilityStateV1::Unavailable,
            Self::EnabledWithoutStack { .. } | Self::Enabled(_) => {
                GitHubStackCapabilityStateV1::Enabled
            }
            Self::Degraded { .. } => GitHubStackCapabilityStateV1::Degraded,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitHubStackObservationV1 {
    pub scope: ResolvedScope,
    pub observed_at: UtcMicros,
    pub capability_anchor_id: RetrievalAnchorId,
    pub capability: GitHubStackCapabilitySnapshotV1,
    pub snapshot_anchor_id: Option<RetrievalAnchorId>,
    pub snapshot: Option<GitHubStackSnapshotV1>,
    pub transitions: Vec<StackSignalKindV1>,
    pub drift_observations: Vec<GitHubStackDriftObservationV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitHubStackDriftObservationV1 {
    pub trace_id: ManifestDigest,
    pub observed_at: UtcMicros,
    pub drift: WorkStackDriftObservedV1,
}

impl GitHubStackDriftObservationV1 {
    pub fn from_persisted(
        scope_digest: &ManifestDigest,
        observed_at: UtcMicros,
        drift: WorkStackDriftObservedV1,
    ) -> Result<Self, GitHubStackCoordinatorErrorV1> {
        let value = Self {
            trace_id: transition::drift_trace_id(
                scope_digest,
                drift.kind,
                UtcMicros(drift.first_observed_micros),
            )?,
            observed_at,
            drift,
        };
        value.validate(scope_digest)?;
        Ok(value)
    }

    pub fn validate(
        &self,
        scope_digest: &ManifestDigest,
    ) -> Result<(), GitHubStackCoordinatorErrorV1> {
        transition::validate_drift_observation(scope_digest, self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitHubStackCoordinatorErrorV1 {
    InvalidScope,
    InvalidProviderObservation,
    Poisoned,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StackCoordinatorErrorV1 {
    Unavailable,
    Saturated,
    Denied,
    Stale,
    Cancelled,
    Invalid(String),
}

#[derive(Default)]
pub struct DaemonGitHubStackCoordinatorV1 {
    policies: Mutex<BTreeMap<ManifestDigest, GitHubStackedPullRequestPolicyV1>>,
    observations: Mutex<BTreeMap<ManifestDigest, GitHubStackObservationV1>>,
    circuit_policies: Mutex<BTreeMap<ManifestDigest, StackCircuitPolicyV1>>,
    dedupe: Mutex<BTreeMap<(ManifestDigest, StackSignalKindV1, ManifestDigest), UtcMicros>>,
    circuits: Mutex<BTreeMap<CircuitKey, Circuit>>,
    preflights: Mutex<PreflightState>,
    drift_intervals: Mutex<BTreeMap<ManifestDigest, Vec<DriftInterval>>>,
}

impl DaemonGitHubStackCoordinatorV1 {
    pub fn register_scope(
        &self,
        scope: &ResolvedScope,
        policy: GitHubStackedPullRequestPolicyV1,
    ) -> Result<(), GitHubStackCoordinatorErrorV1> {
        scope
            .validate()
            .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidScope)?;
        self.policies
            .lock()
            .map_err(|_| GitHubStackCoordinatorErrorV1::Poisoned)?
            .insert(scope.scope_digest.clone(), policy);
        Ok(())
    }

    pub fn should_read_provider_stack(
        &self,
        scope: &ResolvedScope,
    ) -> Result<bool, GitHubStackCoordinatorErrorV1> {
        let policies = self
            .policies
            .lock()
            .map_err(|_| GitHubStackCoordinatorErrorV1::Poisoned)?;
        Ok(matches!(
            policies.get(&scope.scope_digest),
            Some(GitHubStackedPullRequestPolicyV1::ProbePrivatePreview)
        ))
    }

    pub fn observe_policy(
        &self,
        scope: ResolvedScope,
        provider: ProviderId,
        source_binding: GitHubStackProviderSourceBindingV1,
        observed_at: UtcMicros,
    ) -> Result<GitHubStackObservationV1, GitHubStackCoordinatorErrorV1> {
        let state = if self.should_read_provider_stack(&scope)? {
            GitHubStackCapabilityStateV1::Unavailable
        } else {
            GitHubStackCapabilityStateV1::PrivatePreviewDisabled
        };
        if state == GitHubStackCapabilityStateV1::Unavailable
            && let Some(provider_observation) = self
                .observations
                .lock()
                .map_err(|_| GitHubStackCoordinatorErrorV1::Poisoned)?
                .get(&scope.scope_digest)
                .filter(|observation| {
                    matches!(
                        observation.capability.state,
                        GitHubStackCapabilityStateV1::Enabled
                            | GitHubStackCapabilityStateV1::Degraded
                    )
                })
                .cloned()
        {
            return Ok(provider_observation);
        }
        let digest = canonical_sha256(&(
            "tracedecay.github-stack.policy-observation.v1",
            &scope.scope_digest,
            state,
            observed_at,
        ))
        .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidProviderObservation)?;
        self.store_observation(
            scope,
            provider,
            state,
            &digest,
            None,
            &source_binding,
            observed_at,
        )
    }

    pub fn observe_provider(
        &self,
        scope: ResolvedScope,
        provider: ProviderId,
        outcome: GitHubStackProviderOutcomeV1,
        source_binding: GitHubStackProviderSourceBindingV1,
        observed_at: UtcMicros,
    ) -> Result<GitHubStackObservationV1, GitHubStackCoordinatorErrorV1> {
        if !self.should_read_provider_stack(&scope)? {
            return self.observe_policy(scope, provider, source_binding, observed_at);
        }
        let state = outcome.capability_state();
        let response_digest = match outcome.response_digest() {
            Some(digest) => digest.clone(),
            None => canonical_sha256(&(
                "tracedecay.github-stack.unavailable.v1",
                &scope.scope_digest,
                observed_at,
            ))
            .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidProviderObservation)?,
        };
        let snapshot = match outcome {
            GitHubStackProviderOutcomeV1::Enabled(snapshot) => Some(snapshot),
            _ => None,
        };
        self.store_observation(
            scope,
            provider,
            state,
            &response_digest,
            snapshot.as_ref(),
            &source_binding,
            observed_at,
        )
    }

    pub fn fanout_authorized<F>(
        &self,
        mut roots: Vec<ResolvedScope>,
        mut authorize: F,
    ) -> Result<Vec<GitHubStackObservationV1>, GitHubStackCoordinatorErrorV1>
    where
        F: FnMut(&ResolvedScope) -> bool,
    {
        roots.sort_by(|left, right| left.scope_digest.cmp(&right.scope_digest));
        roots.dedup_by(|left, right| left.scope_digest == right.scope_digest);
        roots.truncate(MAX_GITHUB_STACK_ROOTS_PER_FANOUT_V1);
        let observations = self
            .observations
            .lock()
            .map_err(|_| GitHubStackCoordinatorErrorV1::Poisoned)?;
        Ok(roots
            .into_iter()
            .filter(|root| authorize(root))
            .filter_map(|root| observations.get(&root.scope_digest).cloned())
            .take(MAX_GITHUB_STACK_SIGNALS_PER_FANOUT_V1)
            .collect())
    }

    pub fn register_circuit_policy(
        &self,
        scope: &ResolvedScope,
        policy: StackCircuitPolicyV1,
    ) -> Result<(), StackCoordinatorErrorV1> {
        scope.validate().map_err(invalid)?;
        policy.validate()?;
        self.circuit_policies
            .lock()
            .map_err(lock_error)?
            .insert(scope.scope_digest.clone(), policy);
        Ok(())
    }

    pub fn seal_transition(
        &self,
        scope: &ResolvedScope,
        draft: StackSignalDraftV1,
    ) -> Result<StackSignalV1, StackCoordinatorErrorV1> {
        StackSignalV1::seal(scope, draft)
    }

    pub fn restore_open_drift_interval(
        &self,
        scope: &ResolvedScope,
        observation: &GitHubStackDriftObservationV1,
    ) -> Result<(), GitHubStackCoordinatorErrorV1> {
        transition::restore_open_drift_interval(&self.drift_intervals, scope, observation)
    }

    pub fn enqueue_transition<S: StackCoordinatorStore, A: StackDeliveryAuthorizationPort>(
        &self,
        store: &S,
        authorization: &A,
        scope: &ResolvedScope,
        signal: StackSignalV1,
    ) -> Result<(), StackCoordinatorErrorV1> {
        signal.validate(scope)?;
        let mut recipients = authorization.select_recipients(scope, &signal)?;
        let requested = recipients.len();
        recipients.sort();
        recipients.dedup();
        let mut authorized = Vec::new();
        let mut denied = false;
        let mut stale = false;
        for recipient in recipients {
            recipient.validate().map_err(invalid)?;
            match authorization.authorize(&recipient, &signal) {
                StackDeliveryAuthorizationV1::Authorized => authorized.push(recipient),
                StackDeliveryAuthorizationV1::Denied => denied = true,
                StackDeliveryAuthorizationV1::Stale => stale = true,
                StackDeliveryAuthorizationV1::Unavailable => {
                    return Err(StackCoordinatorErrorV1::Unavailable);
                }
            }
        }
        if requested > 0 && authorized.is_empty() {
            if stale {
                return Err(StackCoordinatorErrorV1::Stale);
            }
            if denied {
                return Err(StackCoordinatorErrorV1::Denied);
            }
        }
        if signal.kind.is_material() {
            return store.append_signal(signal, authorized);
        }
        let key = (
            signal.scope_digest.clone(),
            signal.kind,
            signal.state_digest.clone(),
        );
        let mut dedupe = self.dedupe.lock().map_err(lock_error)?;
        dedupe.retain(|_, seen| signal.observed_at.0.saturating_sub(seen.0) <= DEDUPE_TTL_MICROS);
        if dedupe.contains_key(&key) {
            return Ok(());
        }
        store.append_signal(signal.clone(), authorized)?;
        dedupe.insert(key, signal.observed_at);
        Ok(())
    }

    pub fn drain_due<
        S: StackCoordinatorStore,
        A: StackDeliveryAuthorizationPort,
        D: StackDeliveryPort,
    >(
        &self,
        store: &S,
        authorization: &A,
        delivery: &D,
        now: UtcMicros,
    ) -> Result<usize, StackCoordinatorErrorV1> {
        let mut pending = store.pending_deliveries()?;
        pending.retain(|(_, signal)| {
            signal.kind.is_material()
                || signal
                    .observed_at
                    .0
                    .saturating_add(signal.kind.debounce_micros())
                    <= now.0
        });
        pending.sort_by(|(ld, ls), (rd, rs)| {
            (ls.observed_at, &ls.signal_id, &ld.recipient).cmp(&(
                rs.observed_at,
                &rs.signal_id,
                &rd.recipient,
            ))
        });
        let mut delivered = 0;
        while !pending.is_empty() {
            let watermark = pending[0].1.watermark_id.clone();
            let mut recipients = BTreeSet::new();
            let mut signal_ids = BTreeSet::new();
            let mut deliveries = Vec::new();
            let mut acknowledged = Vec::new();
            let mut signals = BTreeMap::new();
            let mut consumed = 0;
            for (item, signal) in &pending {
                if signal.watermark_id != watermark
                    || (recipients.len() >= MAX_BATCH_RECIPIENTS
                        && !recipients.contains(&item.recipient))
                    || (signal_ids.len() >= MAX_BATCH_SIGNALS
                        && !signal_ids.contains(&item.signal_id))
                {
                    break;
                }
                consumed += 1;
                match authorization.authorize(&item.recipient, signal) {
                    StackDeliveryAuthorizationV1::Authorized => {
                        recipients.insert(item.recipient.clone());
                        signal_ids.insert(item.signal_id.clone());
                        deliveries.push(item.clone());
                        acknowledged.push(item.clone());
                        signals
                            .entry(signal.signal_id.clone())
                            .or_insert_with(|| signal.clone());
                    }
                    outcome @ (StackDeliveryAuthorizationV1::Denied
                    | StackDeliveryAuthorizationV1::Stale) => {
                        store.record_authorization_loss(signal, &item.recipient, outcome)?;
                        acknowledged.push(item.clone())
                    }
                    StackDeliveryAuthorizationV1::Unavailable => {}
                }
            }
            if consumed == 0 {
                return Err(StackCoordinatorErrorV1::Invalid(
                    "fanout batch made no progress".to_owned(),
                ));
            }
            let batch = StackDeliveryBatchV1 {
                watermark_id: watermark.clone(),
                recipients: recipients.into_iter().collect(),
                signals: signals.into_values().collect(),
                deliveries,
                partial: consumed < pending.len(),
            };
            if !batch.recipients.is_empty() {
                delivery.deliver(&batch)?;
                delivered += batch.deliveries.len();
            }
            store.acknowledge(&watermark, &acknowledged)?;
            pending.drain(..consumed);
        }
        Ok(delivered)
    }

    pub fn expand_transition<S: StackCoordinatorStore, A: StackDeliveryAuthorizationPort>(
        &self,
        store: &S,
        authorization: &A,
        recipient: &ActorId,
        signal_id: &StackSignalId,
    ) -> Result<Option<StackSignalV1>, StackCoordinatorErrorV1> {
        let Some(signal) = store.signal(signal_id)? else {
            return Ok(None);
        };
        match authorization.authorize(recipient, &signal) {
            StackDeliveryAuthorizationV1::Authorized => Ok(Some(signal)),
            StackDeliveryAuthorizationV1::Denied => Err(StackCoordinatorErrorV1::Denied),
            StackDeliveryAuthorizationV1::Stale => Err(StackCoordinatorErrorV1::Stale),
            StackDeliveryAuthorizationV1::Unavailable => Err(StackCoordinatorErrorV1::Unavailable),
        }
    }

    pub fn optional_preflight<P: OptionalStackPreflightPort>(
        &self,
        preflight: &P,
        request: &NativeIntegrationPreflightRequestV1,
        cancellation: &CancellationSignal,
        now: UtcMicros,
    ) -> Result<OptionalPreflightDispositionV1, StackCoordinatorErrorV1> {
        let revision_digest = match &request.topology.selection {
            tracedecay_application::NativeIntegrationSelectionBindingV1::DeclaredStackEdge {
                revision_digest,
                ..
            } => revision_digest.clone(),
            tracedecay_application::NativeIntegrationSelectionBindingV1::IndependentBranch {
                ..
            } => return Ok(OptionalPreflightDispositionV1::Denied),
        };
        let key = PreflightKey {
            repository_id: request.topology.destination.repository_id.clone(),
            scope_digest: request.topology.destination.scope_digest.clone(),
            revision_digest,
            request_digest: canonical_sha256(request).map_err(invalid)?,
        };
        let circuit_key = CircuitKey {
            repository_id: key.repository_id.clone(),
            scope_digest: key.scope_digest.clone(),
        };
        let policy = self
            .circuit_policies
            .lock()
            .map_err(lock_error)?
            .get(&key.scope_digest)
            .cloned()
            .ok_or(StackCoordinatorErrorV1::Unavailable)?;
        if !self.admit_circuit(&circuit_key, now)? {
            return Ok(OptionalPreflightDispositionV1::SuppressedOpenCircuit);
        }
        let (flight, owner) = {
            let mut state = self.preflights.lock().map_err(lock_error)?;
            if let Some(flight) = state.in_flight.get(&key) {
                (Arc::clone(flight), false)
            } else {
                if state.daemon_active >= MAX_DAEMON_PREFLIGHTS
                    || state
                        .repository_active
                        .get(&key.repository_id)
                        .copied()
                        .unwrap_or(0)
                        >= MAX_REPOSITORY_PREFLIGHTS
                {
                    drop(state);
                    self.fail_circuit(&circuit_key, &policy, now)?;
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
                state.in_flight.insert(key.clone(), Arc::clone(&flight));
                (flight, true)
            }
        };
        if !owner {
            let mut result = flight.result.lock().map_err(lock_error)?;
            let deadline = Instant::now() + PREFLIGHT_JOIN_TIMEOUT;
            while result.is_none() {
                if cancellation.is_cancelled() {
                    return Err(StackCoordinatorErrorV1::Cancelled);
                }
                let now_instant = Instant::now();
                if now_instant >= deadline {
                    drop(result);
                    self.fail_circuit(&circuit_key, &policy, now)?;
                    return Err(StackCoordinatorErrorV1::Unavailable);
                }
                result = flight
                    .complete
                    .wait_timeout(
                        result,
                        PREFLIGHT_JOIN_WAIT_SLICE.min(deadline - now_instant),
                    )
                    .map_err(lock_error)?
                    .0;
            }
            return result.clone().ok_or_else(|| {
                StackCoordinatorErrorV1::Invalid("missing joined preflight result".to_owned())
            })?;
        }
        let result = preflight
            .preflight(request, cancellation)
            .map(classify_preflight);
        *flight.result.lock().map_err(lock_error)? = Some(result.clone());
        flight.complete.notify_all();
        let mut state = self.preflights.lock().map_err(lock_error)?;
        state.in_flight.remove(&key);
        state.daemon_active = state.daemon_active.saturating_sub(1);
        if let Some(active) = state.repository_active.get_mut(&key.repository_id) {
            *active = active.saturating_sub(1);
            if *active == 0 {
                state.repository_active.remove(&key.repository_id);
            }
        }
        drop(state);
        match &result {
            Ok(OptionalPreflightDispositionV1::Complete) => {
                self.circuits
                    .lock()
                    .map_err(lock_error)?
                    .remove(&circuit_key);
            }
            Ok(
                OptionalPreflightDispositionV1::Partial
                | OptionalPreflightDispositionV1::Cancelled
                | OptionalPreflightDispositionV1::Unavailable,
            )
            | Err(
                StackCoordinatorErrorV1::Cancelled
                | StackCoordinatorErrorV1::Unavailable
                | StackCoordinatorErrorV1::Saturated,
            ) => self.fail_circuit(&circuit_key, &policy, now)?,
            Ok(_)
            | Err(
                StackCoordinatorErrorV1::Denied
                | StackCoordinatorErrorV1::Stale
                | StackCoordinatorErrorV1::Invalid(_),
            ) => self.reopen_half_open(&circuit_key, &policy, now)?,
        }
        result
    }

    fn admit_circuit(
        &self,
        key: &CircuitKey,
        now: UtcMicros,
    ) -> Result<bool, StackCoordinatorErrorV1> {
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

    fn fail_circuit(
        &self,
        key: &CircuitKey,
        policy: &StackCircuitPolicyV1,
        now: UtcMicros,
    ) -> Result<(), StackCoordinatorErrorV1> {
        let mut circuits = self.circuits.lock().map_err(lock_error)?;
        let circuit = circuits.entry(key.clone()).or_insert(Circuit {
            state: CircuitState::Closed,
            failures: 0,
        });
        circuit.failures = circuit.failures.saturating_add(1);
        if circuit.failures >= policy.failure_threshold
            || circuit.state == CircuitState::HalfOpenProbe
        {
            circuit.state = CircuitState::Open {
                until: UtcMicros(now.0.saturating_add(policy.open_micros)),
            };
        }
        Ok(())
    }

    fn reopen_half_open(
        &self,
        key: &CircuitKey,
        policy: &StackCircuitPolicyV1,
        now: UtcMicros,
    ) -> Result<(), StackCoordinatorErrorV1> {
        let mut circuits = self.circuits.lock().map_err(lock_error)?;
        if let Some(circuit) = circuits.get_mut(key)
            && circuit.state == CircuitState::HalfOpenProbe
        {
            circuit.state = CircuitState::Open {
                until: UtcMicros(now.0.saturating_add(policy.open_micros)),
            };
        }
        Ok(())
    }

    fn store_observation(
        &self,
        scope: ResolvedScope,
        provider: ProviderId,
        state: GitHubStackCapabilityStateV1,
        response_digest: &ManifestDigest,
        provider_snapshot: Option<&GitHubStackProviderSnapshotV1>,
        source_binding: &GitHubStackProviderSourceBindingV1,
        observed_at: UtcMicros,
    ) -> Result<GitHubStackObservationV1, GitHubStackCoordinatorErrorV1> {
        scope
            .validate()
            .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidScope)?;
        provider
            .validate()
            .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidProviderObservation)?;
        response_digest
            .validate()
            .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidProviderObservation)?;
        source_binding
            .owner
            .validate()
            .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidProviderObservation)?;
        if source_binding.owner.project_id() != Some(&scope.project_id) {
            return Err(GitHubStackCoordinatorErrorV1::InvalidProviderObservation);
        }
        source_binding
            .capability_source_anchor_id
            .validate()
            .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidProviderObservation)?;
        source_binding
            .snapshot_source_anchor_id
            .as_ref()
            .map_or(Ok(()), RetrievalAnchorId::validate)
            .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidProviderObservation)?;
        if provider_snapshot.is_some() != source_binding.snapshot_source_anchor_id.is_some() {
            return Err(GitHubStackCoordinatorErrorV1::InvalidProviderObservation);
        }
        let generation_id = generation_id(response_digest)?;
        let capability = GitHubStackCapabilitySnapshotV1::new(
            provider.clone(),
            scope.project_id.clone(),
            scope.repository_id.clone(),
            scope.worktree_id.clone(),
            state,
            generation_id.clone(),
            source_binding.capability_source_anchor_id.clone(),
        )
        .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidProviderObservation)?;
        let anchor_owner = ObservationScopeV1::Project {
            project_id: scope.project_id.clone(),
        };
        let capability_anchor_id = derive_git_topology_anchor_id(
            &anchor_owner,
            &GitTopologyAnchorTargetV1::GitHubStackCapability(capability.clone()),
        )
        .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidProviderObservation)?;
        let snapshot = provider_snapshot
            .map(|snapshot| {
                build_snapshot(
                    &scope,
                    &provider,
                    &capability,
                    &generation_id,
                    snapshot,
                    source_binding
                        .snapshot_source_anchor_id
                        .as_ref()
                        .ok_or(GitHubStackCoordinatorErrorV1::InvalidProviderObservation)?,
                )
            })
            .transpose()?;
        let snapshot_anchor_id = snapshot
            .as_ref()
            .map(|snapshot| {
                derive_git_topology_anchor_id(
                    &anchor_owner,
                    &GitTopologyAnchorTargetV1::GitHubStackSnapshot(snapshot.clone()),
                )
            })
            .transpose()
            .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidProviderObservation)?;
        let previous = self
            .observations
            .lock()
            .map_err(|_| GitHubStackCoordinatorErrorV1::Poisoned)?
            .get(&scope.scope_digest)
            .and_then(|observation| observation.snapshot.as_ref())
            .cloned();
        let (transitions, drift_kinds) = stack_drift(previous.as_ref(), snapshot.as_ref());
        let drift_observations = transition::update_drift_intervals(
            &self.drift_intervals,
            &scope.scope_digest,
            drift_kinds,
            observed_at,
        )?;
        let observation = GitHubStackObservationV1 {
            scope: scope.clone(),
            observed_at,
            capability_anchor_id,
            capability,
            snapshot_anchor_id,
            snapshot,
            transitions,
            drift_observations,
        };
        self.observations
            .lock()
            .map_err(|_| GitHubStackCoordinatorErrorV1::Poisoned)?
            .insert(scope.scope_digest, observation.clone());
        Ok(observation)
    }
}

fn build_snapshot(
    scope: &ResolvedScope,
    provider: &ProviderId,
    capability: &GitHubStackCapabilitySnapshotV1,
    generation_id: &ProjectionGenerationId,
    snapshot: &GitHubStackProviderSnapshotV1,
    source_anchor_id: &RetrievalAnchorId,
) -> Result<GitHubStackSnapshotV1, GitHubStackCoordinatorErrorV1> {
    if snapshot.layers.is_empty() || snapshot.layers.len() > MAX_GITHUB_STACK_LAYERS_V1 {
        return Err(GitHubStackCoordinatorErrorV1::InvalidProviderObservation);
    }
    let layers = snapshot
        .layers
        .iter()
        .map(|layer| {
            let pull_request = layer.pull_request.clone();
            pull_request
                .validate()
                .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidProviderObservation)?;
            if pull_request.provider != *provider
                || pull_request.project_id != scope.project_id
                || pull_request.repository_id != scope.repository_id
                || pull_request.worktree_id != scope.worktree_id
            {
                return Err(GitHubStackCoordinatorErrorV1::InvalidProviderObservation);
            }
            Ok(GitHubStackLayerSnapshotV1 {
                provider_position: layer.provider_position,
                pull_request,
                base_ref_id: layer.base_ref_id.clone(),
                head_ref_id: layer.head_ref_id.clone(),
                protection_digest: layer.protection_digest.clone(),
                ci_digest: layer.ci_digest.clone(),
                merge_queue_digest: layer.merge_queue_digest.clone(),
            })
        })
        .collect::<Result<Vec<_>, GitHubStackCoordinatorErrorV1>>()?;
    GitHubStackSnapshotV1::new(
        capability.clone(),
        snapshot.provider_stack_id_digest.clone(),
        generation_id.clone(),
        snapshot.final_target_ref_id.clone(),
        snapshot.final_target_commit_id.clone(),
        layers,
        source_anchor_id.clone(),
    )
    .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidProviderObservation)
}

fn generation_id(
    digest: &ManifestDigest,
) -> Result<ProjectionGenerationId, GitHubStackCoordinatorErrorV1> {
    ProjectionGenerationId::new(format!(
        "generation.github-stack.{}",
        digest.as_str().replace(':', ".")
    ))
    .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidProviderObservation)
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

fn invalid(error: impl std::fmt::Display) -> StackCoordinatorErrorV1 {
    StackCoordinatorErrorV1::Invalid(error.to_string())
}
fn lock_error<T>(error: std::sync::PoisonError<T>) -> StackCoordinatorErrorV1 {
    invalid(error)
}
