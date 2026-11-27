use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tracedecay_contracts::{
    CancellationSignal, NativeIntegrationPreflightOutcomeV1, NativeIntegrationPreflightRequestV1,
    ResolvedScope,
};
use tracedecay_domain::configuration::GitHubStackedPullRequestPolicyV1;
use tracedecay_domain::{
    ActorId, AnchorOwnerBindingV1, CommitId, GitHubStackCapabilitySnapshotV1,
    GitHubStackCapabilityStateV1, GitHubStackLayerSnapshotV1, GitHubStackSnapshotV1,
    GitTopologyAnchorTargetV1, ManifestDigest, ObservationScopeV1, PrivacyDomainBoundLocatorDigest,
    ProjectionGenerationId, ProviderId, PullRequestSnapshotAnchorRefV1, RefId, RepositoryId,
    RetrievalAnchorId, StackDriftKindV1, StackSignalId, StackSignalKindV1, UtcMicros,
    WorkStackDriftObservedV1, canonical_sha256, derive_git_topology_anchor_id,
};
use tracedecay_runtime_core::native_runtime;

mod transition;
pub use transition::{
    DEDUPE_TTL_MICROS, MAX_DAEMON_PREFLIGHTS, MAX_REPOSITORY_PREFLIGHTS, OptionalPreflightV1,
    OptionalStackPreflightPort, StackCircuitPolicyV1, StackCoordinatorStore,
    StackDeliveryAuthorizationPort, StackDeliveryAuthorizationV1, StackSignalDraftV1,
    StackSignalV1,
};
use transition::{DriftInterval, stack_drift};

pub const MAX_GITHUB_STACK_LAYERS_V1: usize = 100;
const PREFLIGHT_JOIN_WAIT_SLICE: Duration = Duration::from_millis(25);
const PREFLIGHT_JOIN_TIMEOUT: Duration = Duration::from_secs(5);
const STACK_CIRCUIT_FAILURE_THRESHOLD: u32 = 3;
const STACK_CIRCUIT_OPEN_MICROS: i64 = 30_000_000;

/// The preflight circuit policy every mounted stack scope registers. The
/// daemon registers it together with the scope's stacked-PR policy, so one
/// registration owns both and a runtime mount only joins that registration.
pub fn github_stack_circuit_policy() -> Result<StackCircuitPolicyV1, StackCoordinatorErrorV1> {
    StackCircuitPolicyV1 {
        revision: 1,
        policy_digest: ManifestDigest::new(format!("sha256:{}", "0".repeat(64)))
            .map_err(|error| StackCoordinatorErrorV1::Invalid(error.to_string()))?,
        failure_threshold: STACK_CIRCUIT_FAILURE_THRESHOLD,
        open_micros: STACK_CIRCUIT_OPEN_MICROS,
    }
    .seal()
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
    // The flight retains this circuit allocation until settlement. A new
    // registration, even with identical policy bytes, cannot join its result.
    circuit_registration: usize,
}

struct InFlight {
    result: Mutex<Option<Result<NativeIntegrationPreflightOutcomeV1, StackCoordinatorErrorV1>>>,
    complete: Notify,
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

struct GitHubStackObservationMaterial<'a> {
    response_digest: &'a ManifestDigest,
    provider_snapshot: Option<&'a GitHubStackProviderSnapshotV1>,
    source_binding: &'a GitHubStackProviderSourceBindingV1,
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
    /// The canonical trace identity a durable drift receipt carries for one
    /// exact scope. Persisted evidence is bound to the scope digest through
    /// this digest, not through the project-wide receipt scope reference.
    pub fn trace_id_for(
        scope_digest: &ManifestDigest,
        kind: StackDriftKindV1,
        first_observed_at: UtcMicros,
    ) -> Result<ManifestDigest, GitHubStackCoordinatorErrorV1> {
        transition::drift_trace_id(scope_digest, kind, first_observed_at)
    }

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
    InvalidCircuitPolicy,
    /// Restart restoration was offered after this registration had already
    /// published an observation, so its live drift state is newer than any
    /// durable receipt the caller could have read.
    ScopeAlreadyObserved,
    /// The scope registration that authorized the observation was replaced
    /// before it could publish.
    Superseded,
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

/// Identity of one scope registration. Every holder keeps the allocation
/// alive, so a successor registration can never compare equal to a retired
/// one that is still referenced.
#[derive(Clone, Debug)]
struct ScopeRegistrationId(Arc<()>);

impl ScopeRegistrationId {
    fn new() -> Self {
        Self(Arc::new(()))
    }
}

fn same_registration(
    left: Option<&ScopeRegistrationId>,
    right: Option<&ScopeRegistrationId>,
) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => Arc::ptr_eq(&left.0, &right.0),
        (None, None) => true,
        _ => false,
    }
}

#[derive(Clone, Debug)]
struct ScopeRegistration {
    id: ScopeRegistrationId,
    policy: GitHubStackedPullRequestPolicyV1,
}

/// One provider stack read authorized by the scope's current registration.
///
/// Capture it before network I/O and surrender it to
/// [`DaemonGitHubStackCoordinatorV1::observe_provider`] on completion.
/// Publication is refused unless that same registration is still current, so
/// a read spanning a policy change, including a change and its reversal,
/// cannot publish. Dropping it abandons the read without touching state.
pub struct GitHubStackProviderReadV1 {
    scope: Arc<RetainedScope>,
    registration: ScopeRegistrationId,
}

impl std::fmt::Debug for GitHubStackProviderReadV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GitHubStackProviderReadV1")
            .field("scope", &self.scope.scope)
            .finish_non_exhaustive()
    }
}

/// Who derived an observation and under which registration.
enum ObservationOrigin {
    /// Derived from the registered policy alone; it never replaces a provider
    /// result published under the same registration.
    Policy(Option<ScopeRegistrationId>),
    /// A provider read authorized by this registration.
    Provider(ScopeRegistrationId),
}

impl ObservationOrigin {
    fn registration(&self) -> Option<&ScopeRegistrationId> {
        match self {
            Self::Policy(registration) => registration.as_ref(),
            Self::Provider(registration) => Some(registration),
        }
    }
}

struct PublishedObservation {
    registration: Option<ScopeRegistrationId>,
    observation: GitHubStackObservationV1,
}

/// All retained state belongs to one mounted scope. The coordinator only
/// keeps weak references; accepted provider reads and preflights retain the
/// same scope until their work settles.
#[derive(Default)]
struct ScopeObservationState {
    registration: Option<ScopeRegistration>,
    current: Option<PublishedObservation>,
    drift_intervals: Vec<DriftInterval>,
    circuit: Option<Arc<ScopeCircuit>>,
}

struct ScopeCircuit {
    policy: StackCircuitPolicyV1,
    status: Mutex<Circuit>,
}

type ScopeRegistry = Mutex<BTreeMap<ManifestDigest, Weak<MountedScope>>>;

struct RetainedScope {
    scope: ResolvedScope,
    state: Mutex<ScopeObservationState>,
    // Serialize a scope's debounced appends, not every project's store I/O.
    dedupe: tokio::sync::Mutex<BTreeMap<(StackSignalKindV1, ManifestDigest), UtcMicros>>,
}

/// Mounted owners share this token. Accepted operations retain only its
/// state, so a draining operation cannot extend the mounted registration or
/// let a later mount inherit its policy, observations or circuit.
struct MountedScope {
    retained: Arc<RetainedScope>,
    registry: Weak<ScopeRegistry>,
}

impl Drop for MountedScope {
    fn drop(&mut self) {
        let Some(registry) = self.registry.upgrade() else {
            return;
        };
        let mut scopes = match registry.lock() {
            Ok(scopes) => scopes,
            Err(error) => {
                tracing::warn!("GitHub stack scope retirement recovered a poisoned registry");
                error.into_inner()
            }
        };
        if scopes
            .get(&self.retained.scope.scope_digest)
            .is_some_and(|entry| std::ptr::eq(entry.as_ptr(), self))
        {
            scopes.remove(&self.retained.scope.scope_digest);
        }
    }
}

/// A mounted lifetime owns this handle; accepted work retains its state only.
/// Retiring an old handle cannot remove a replacement scope registration.
#[derive(Clone)]
#[must_use = "retain the scope handle for the mounted runtime lifetime"]
pub struct GitHubStackScopeV1(Arc<MountedScope>);

impl GitHubStackScopeV1 {
    /// The exact scope this registration was mounted for.
    pub fn scope(&self) -> &ResolvedScope {
        &self.0.retained.scope
    }

    /// Whether this registration has published no observation yet, which is
    /// the only state in which restart restoration may still supply durable
    /// drift intervals. Once an observation exists, the scope's live drift
    /// state is newer than any receipt a restarted read can see.
    pub fn is_mount_fresh(&self) -> Result<bool, GitHubStackCoordinatorErrorV1> {
        Ok(self
            .0
            .retained
            .state
            .lock()
            .map_err(|_| GitHubStackCoordinatorErrorV1::Poisoned)?
            .current
            .is_none())
    }
}

impl std::fmt::Debug for GitHubStackScopeV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("GitHubStackScopeV1")
            .field(&self.0.retained.scope)
            .finish()
    }
}

#[derive(Default)]
pub struct DaemonGitHubStackCoordinatorV1 {
    scopes: Arc<ScopeRegistry>,
    preflights: Mutex<PreflightState>,
}

impl DaemonGitHubStackCoordinatorV1 {
    pub fn retain_scope(
        &self,
        scope: &ResolvedScope,
    ) -> Result<GitHubStackScopeV1, GitHubStackCoordinatorErrorV1> {
        scope
            .validate()
            .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidScope)?;
        let mut scopes = self
            .scopes
            .lock()
            .map_err(|_| GitHubStackCoordinatorErrorV1::Poisoned)?;
        if let Some(retained) = scopes.get(&scope.scope_digest).and_then(Weak::upgrade) {
            if retained.retained.scope != *scope {
                drop(scopes);
                return Err(GitHubStackCoordinatorErrorV1::InvalidScope);
            }
            return Ok(GitHubStackScopeV1(retained));
        }
        let retained = Arc::new(MountedScope {
            retained: Arc::new(RetainedScope {
                scope: scope.clone(),
                state: Mutex::new(ScopeObservationState::default()),
                dedupe: tokio::sync::Mutex::new(BTreeMap::new()),
            }),
            registry: Arc::downgrade(&self.scopes),
        });
        scopes.insert(scope.scope_digest.clone(), Arc::downgrade(&retained));
        Ok(GitHubStackScopeV1(retained))
    }

    /// Mounts the one registration a real owner holds for its lifetime. The
    /// same registration owns the stacked-PR policy, the preflight circuit
    /// policy and their state, so callers never join several independently
    /// keyed maps. Re-registering the same policy keeps its epoch and any
    /// in-flight read that already captured it.
    pub fn register_scope(
        &self,
        scope: &ResolvedScope,
        policy: GitHubStackedPullRequestPolicyV1,
        circuit_policy: StackCircuitPolicyV1,
    ) -> Result<GitHubStackScopeV1, GitHubStackCoordinatorErrorV1> {
        circuit_policy.validate().map_err(invalid_circuit)?;
        let retained = self.retain_scope(scope)?;
        {
            let mut state = retained
                .0
                .retained
                .state
                .lock()
                .map_err(|_| GitHubStackCoordinatorErrorV1::Poisoned)?;
            // A changed policy invalidates earlier reads, including a change
            // followed by its reversal. An unchanged policy retains its epoch.
            if state
                .registration
                .as_ref()
                .is_none_or(|registration| registration.policy != policy)
            {
                state.registration = Some(ScopeRegistration {
                    id: ScopeRegistrationId::new(),
                    policy,
                });
            }
            if state
                .circuit
                .as_ref()
                .is_none_or(|circuit| circuit.policy != circuit_policy)
            {
                state.circuit = Some(Arc::new(ScopeCircuit {
                    policy: circuit_policy,
                    status: Mutex::new(Circuit {
                        state: CircuitState::Closed,
                        failures: 0,
                    }),
                }));
            }
        }
        Ok(retained)
    }

    /// Durable restart restoration of one persisted open drift interval.
    ///
    /// Restoration is a mount transition. It must be applied through a live
    /// registration so a restored interval cannot outlive its owner or land in
    /// a retired slot, and it is refused once this registration has published
    /// an observation: a rebuilt owner re-reading older durable receipts may
    /// not overwrite drift state this process already observed. Within the
    /// fresh mount, the earlier opening time wins so repeated reads of the
    /// same receipts are idempotent.
    pub fn restore_open_drift_interval(
        &self,
        registration: &GitHubStackScopeV1,
        observation: &GitHubStackDriftObservationV1,
    ) -> Result<(), GitHubStackCoordinatorErrorV1> {
        let scope = registration.scope();
        let interval = transition::restored_open_interval(scope, observation)?;
        let mut state = registration
            .0
            .retained
            .state
            .lock()
            .map_err(|_| GitHubStackCoordinatorErrorV1::Poisoned)?;
        if state.current.is_some() {
            return Err(GitHubStackCoordinatorErrorV1::ScopeAlreadyObserved);
        }
        match state
            .drift_intervals
            .iter_mut()
            .find(|existing| existing.kind == interval.kind)
        {
            Some(existing) if existing.first_observed_at > interval.first_observed_at => {
                *existing = interval;
            }
            Some(_) => {}
            None => state.drift_intervals.push(interval),
        }
        Ok(())
    }

    /// Capture authorization and the scope lifetime before network I/O.
    pub fn authorize_provider_read(
        &self,
        scope: &ResolvedScope,
    ) -> Result<Option<GitHubStackProviderReadV1>, GitHubStackCoordinatorErrorV1> {
        let retained = self.retain_scope(scope)?;
        let registration = retained
            .0
            .retained
            .state
            .lock()
            .map_err(|_| GitHubStackCoordinatorErrorV1::Poisoned)?
            .registration
            .clone();
        Ok(registration
            .filter(|registration| {
                registration.policy == GitHubStackedPullRequestPolicyV1::ProbePrivatePreview
            })
            .map(|registration| GitHubStackProviderReadV1 {
                scope: Arc::clone(&retained.0.retained),
                registration: registration.id,
            }))
    }

    fn registration(
        &self,
        scope: &ResolvedScope,
    ) -> Result<Option<ScopeRegistration>, GitHubStackCoordinatorErrorV1> {
        let retained = self.retain_scope(scope)?;
        let registration = retained
            .0
            .retained
            .state
            .lock()
            .map_err(|_| GitHubStackCoordinatorErrorV1::Poisoned)?
            .registration
            .clone();
        Ok(registration)
    }

    pub fn observe_policy(
        &self,
        scope: ResolvedScope,
        provider: ProviderId,
        source_binding: GitHubStackProviderSourceBindingV1,
        observed_at: UtcMicros,
    ) -> Result<GitHubStackObservationV1, GitHubStackCoordinatorErrorV1> {
        let registration = self.registration(&scope)?;
        let state = if registration.as_ref().is_some_and(|registration| {
            registration.policy == GitHubStackedPullRequestPolicyV1::ProbePrivatePreview
        }) {
            GitHubStackCapabilityStateV1::Unavailable
        } else {
            GitHubStackCapabilityStateV1::PrivatePreviewDisabled
        };
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
            GitHubStackObservationMaterial {
                response_digest: &digest,
                provider_snapshot: None,
                source_binding: &source_binding,
            },
            observed_at,
            ObservationOrigin::Policy(registration.map(|registration| registration.id)),
        )
    }

    /// Publishes the completion of `read`. A read whose registration was
    /// replaced while it ran is refused as `Superseded` and changes nothing.
    pub fn observe_provider(
        &self,
        read: GitHubStackProviderReadV1,
        provider: ProviderId,
        outcome: GitHubStackProviderOutcomeV1,
        source_binding: GitHubStackProviderSourceBindingV1,
        observed_at: UtcMicros,
    ) -> Result<GitHubStackObservationV1, GitHubStackCoordinatorErrorV1> {
        let GitHubStackProviderReadV1 {
            scope,
            registration,
        } = read;
        let resolved_scope = scope.scope.clone();
        let state = outcome.capability_state();
        let response_digest = match outcome.response_digest() {
            Some(digest) => digest.clone(),
            None => canonical_sha256(&(
                "tracedecay.github-stack.unavailable.v1",
                &resolved_scope.scope_digest,
                observed_at,
            ))
            .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidProviderObservation)?,
        };
        let snapshot = match outcome {
            GitHubStackProviderOutcomeV1::Enabled(snapshot) => Some(snapshot),
            _ => None,
        };
        self.store_observation(
            resolved_scope,
            provider,
            state,
            GitHubStackObservationMaterial {
                response_digest: &response_digest,
                provider_snapshot: snapshot.as_ref(),
                source_binding: &source_binding,
            },
            observed_at,
            ObservationOrigin::Provider(registration),
        )
    }

    /// Live registrations only. Accepted provider reads and preflights retain
    /// the same state without a registry entry, so this proves retirement of
    /// the mounted owners rather than of settled work.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn retained_scope_count(&self) -> usize {
        self.scopes
            .lock()
            .map(|scopes| {
                scopes
                    .values()
                    .filter(|entry| entry.strong_count() > 0)
                    .count()
            })
            .unwrap_or(0)
    }

    #[tracing::instrument(name = "usecases.stack.enqueue", level = "trace", skip_all)]
    pub async fn enqueue_transition<S: StackCoordinatorStore, A: StackDeliveryAuthorizationPort>(
        &self,
        store: &S,
        authorization: &A,
        scope: &ResolvedScope,
        signal: StackSignalV1,
    ) -> Result<(), StackCoordinatorErrorV1> {
        signal.validate(scope)?;
        // Capture this invocation's state before authorization can suspend.
        // A remount while it waits must not inherit this append's dedupe key.
        let retained = if signal.kind.is_material() {
            None
        } else {
            Some(Arc::clone(
                &self
                    .retain_scope(scope)
                    .map_err(|error| invalid(format!("{error:?}")))?
                    .0
                    .retained,
            ))
        };
        let mut recipients = authorization.select_recipients(scope, &signal)?;
        let requested = recipients.len();
        recipients.sort();
        recipients.dedup();
        let mut authorized = Vec::new();
        let mut denied = false;
        let mut stale = false;
        for recipient in recipients {
            recipient.validate().map_err(invalid)?;
            match authorization.authorize(&recipient, &signal).await {
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
        let Some(retained) = retained else {
            return store.append_signal(signal, authorized).await;
        };
        let key = (signal.kind, signal.state_digest.clone());
        let mut dedupe = retained.dedupe.lock().await;
        dedupe.retain(|_, seen| signal.observed_at.0.saturating_sub(seen.0) <= DEDUPE_TTL_MICROS);
        if dedupe.contains_key(&key) {
            return Ok(());
        }
        store.append_signal(signal.clone(), authorized).await?;
        dedupe.insert(key, signal.observed_at);
        Ok(())
    }

    #[tracing::instrument(name = "usecases.stack.expand", level = "trace", skip_all)]
    pub async fn expand_transition<S: StackCoordinatorStore, A: StackDeliveryAuthorizationPort>(
        &self,
        store: &S,
        authorization: &A,
        recipient: &ActorId,
        signal_id: &StackSignalId,
    ) -> Result<Option<StackSignalV1>, StackCoordinatorErrorV1> {
        let Some(signal) = store.signal(signal_id).await? else {
            return Ok(None);
        };
        match authorization.authorize(recipient, &signal).await {
            StackDeliveryAuthorizationV1::Authorized => Ok(Some(signal)),
            outcome @ (StackDeliveryAuthorizationV1::Denied
            | StackDeliveryAuthorizationV1::Stale) => {
                store
                    .record_authorization_loss(&signal, recipient, outcome)
                    .await?;
                Err(if outcome == StackDeliveryAuthorizationV1::Denied {
                    StackCoordinatorErrorV1::Denied
                } else {
                    StackCoordinatorErrorV1::Stale
                })
            }
            StackDeliveryAuthorizationV1::Unavailable => Err(StackCoordinatorErrorV1::Unavailable),
        }
    }

    /// Runs `preflight` through the scope's circuit and bounded admission.
    /// Identical in-flight requests share one execution: the owner's full
    /// result, including its preview, reaches every joined waiter.
    pub async fn optional_preflight<P: OptionalStackPreflightPort>(
        &self,
        preflight: &P,
        request: &NativeIntegrationPreflightRequestV1,
        cancellation: &CancellationSignal,
        now: UtcMicros,
    ) -> Result<OptionalPreflightV1, StackCoordinatorErrorV1> {
        let revision_digest = match &request.topology.selection {
            tracedecay_contracts::NativeIntegrationSelectionBindingV1::DeclaredStackEdge {
                revision_digest,
                ..
            } => revision_digest.clone(),
            tracedecay_contracts::NativeIntegrationSelectionBindingV1::IndependentBranch {
                ..
            } => {
                return Ok(OptionalPreflightV1::Completed(
                    NativeIntegrationPreflightOutcomeV1::Denied,
                ));
            }
        };
        let scope = Arc::clone(
            &self
                .retain_scope(&request.topology.destination)
                .map_err(|error| invalid(format!("{error:?}")))?
                .0
                .retained,
        );
        let circuit = scope
            .state
            .lock()
            .map_err(lock_error)?
            .circuit
            .clone()
            .ok_or(StackCoordinatorErrorV1::Unavailable)?;
        let key = PreflightKey {
            repository_id: request.topology.destination.repository_id.clone(),
            scope_digest: request.topology.destination.scope_digest.clone(),
            revision_digest,
            request_digest: canonical_sha256(request).map_err(invalid)?,
            circuit_registration: Arc::as_ptr(&circuit) as usize,
        };
        if !Self::admit_circuit(&circuit, now)? {
            return Ok(OptionalPreflightV1::SuppressedOpenCircuit);
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
                    Self::fail_circuit(&circuit, now)?;
                    return Ok(OptionalPreflightV1::Saturated);
                }
                state.daemon_active += 1;
                *state
                    .repository_active
                    .entry(key.repository_id.clone())
                    .or_default() += 1;
                let flight = Arc::new(InFlight {
                    result: Mutex::new(None),
                    complete: Notify::new(),
                });
                state.in_flight.insert(key.clone(), Arc::clone(&flight));
                (flight, true)
            }
        };
        if !owner {
            let deadline = Instant::now() + PREFLIGHT_JOIN_TIMEOUT;
            loop {
                // Registered before the result is read, so a publication
                // between the read and the wait still wakes this joiner.
                let completed = flight.complete.notified();
                if let Some(result) = flight.result.lock().map_err(lock_error)?.clone() {
                    return result.map(OptionalPreflightV1::Completed);
                }
                if cancellation.is_cancelled() {
                    return Err(StackCoordinatorErrorV1::Cancelled);
                }
                let now_instant = Instant::now();
                if now_instant >= deadline {
                    Self::fail_circuit(&circuit, now)?;
                    return Err(StackCoordinatorErrorV1::Unavailable);
                }
                let _ = native_runtime::timeout(
                    PREFLIGHT_JOIN_WAIT_SLICE.min(deadline - now_instant),
                    completed,
                )
                .await;
            }
        }
        let owned = OwnedFlight {
            coordinator: self,
            key,
            flight,
            _scope: scope,
            circuit,
            now,
            finished: false,
        };
        let result = preflight.preflight(request, cancellation).await;
        owned.finish(result).map(OptionalPreflightV1::Completed)
    }

    fn admit_circuit(
        circuit: &ScopeCircuit,
        now: UtcMicros,
    ) -> Result<bool, StackCoordinatorErrorV1> {
        let mut status = circuit.status.lock().map_err(lock_error)?;
        match status.state {
            CircuitState::Closed => Ok(true),
            CircuitState::Open { until } if now.0 >= until.0 => {
                status.state = CircuitState::HalfOpenProbe;
                Ok(true)
            }
            CircuitState::Open { .. } | CircuitState::HalfOpenProbe => Ok(false),
        }
    }

    fn fail_circuit(circuit: &ScopeCircuit, now: UtcMicros) -> Result<(), StackCoordinatorErrorV1> {
        let mut status = circuit.status.lock().map_err(lock_error)?;
        status.failures = status.failures.saturating_add(1);
        if status.failures >= circuit.policy.failure_threshold
            || status.state == CircuitState::HalfOpenProbe
        {
            status.state = CircuitState::Open {
                until: UtcMicros(now.0.saturating_add(circuit.policy.open_micros)),
            };
        }
        Ok(())
    }

    fn reopen_half_open(
        circuit: &ScopeCircuit,
        now: UtcMicros,
    ) -> Result<(), StackCoordinatorErrorV1> {
        let mut status = circuit.status.lock().map_err(lock_error)?;
        if status.state == CircuitState::HalfOpenProbe {
            status.state = CircuitState::Open {
                until: UtcMicros(now.0.saturating_add(circuit.policy.open_micros)),
            };
        }
        Ok(())
    }

    fn store_observation(
        &self,
        scope: ResolvedScope,
        provider: ProviderId,
        state: GitHubStackCapabilityStateV1,
        material: GitHubStackObservationMaterial<'_>,
        observed_at: UtcMicros,
        origin: ObservationOrigin,
    ) -> Result<GitHubStackObservationV1, GitHubStackCoordinatorErrorV1> {
        scope
            .validate()
            .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidScope)?;
        provider
            .validate()
            .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidProviderObservation)?;
        material
            .response_digest
            .validate()
            .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidProviderObservation)?;
        material
            .source_binding
            .owner
            .validate()
            .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidProviderObservation)?;
        if material.source_binding.owner.project_id() != Some(&scope.project_id) {
            return Err(GitHubStackCoordinatorErrorV1::InvalidProviderObservation);
        }
        material
            .source_binding
            .capability_source_anchor_id
            .validate()
            .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidProviderObservation)?;
        material
            .source_binding
            .snapshot_source_anchor_id
            .as_ref()
            .map_or(Ok(()), RetrievalAnchorId::validate)
            .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidProviderObservation)?;
        if material.provider_snapshot.is_some()
            != material.source_binding.snapshot_source_anchor_id.is_some()
        {
            return Err(GitHubStackCoordinatorErrorV1::InvalidProviderObservation);
        }
        let generation_id = generation_id(material.response_digest)?;
        let capability = GitHubStackCapabilitySnapshotV1::new(
            provider.clone(),
            scope.project_id.clone(),
            scope.repository_id.clone(),
            scope.worktree_id.clone(),
            state,
            generation_id.clone(),
            material.source_binding.capability_source_anchor_id.clone(),
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
        let snapshot = material
            .provider_snapshot
            .map(|snapshot| {
                build_snapshot(
                    &scope,
                    &provider,
                    &capability,
                    &generation_id,
                    snapshot,
                    material
                        .source_binding
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
        // Serialize comparison, interval update, and snapshot publication under
        // one scope-state transition. No network or storage I/O occurs while held.
        let retained = self.retain_scope(&scope)?;
        let mut state = retained
            .0
            .retained
            .state
            .lock()
            .map_err(|_| GitHubStackCoordinatorErrorV1::Poisoned)?;
        // The observation was derived under the registration it names. Policy
        // and provider authority both end when that registration is replaced.
        if !same_registration(
            state
                .registration
                .as_ref()
                .map(|registration| &registration.id),
            origin.registration(),
        ) {
            return Err(GitHubStackCoordinatorErrorV1::Superseded);
        }
        // Only an observation published under this same registration is a
        // drift baseline or a provider result worth preserving.
        let registered_current = state.current.as_ref().filter(|published| {
            same_registration(published.registration.as_ref(), origin.registration())
        });
        // A policy observation prepared before a real provider result of the
        // same registration must not overwrite its enabled/degraded result.
        if let ObservationOrigin::Policy(_) = origin
            && let Some(previous) = registered_current.filter(|published| {
                matches!(
                    published.observation.capability.state,
                    GitHubStackCapabilityStateV1::Enabled | GitHubStackCapabilityStateV1::Degraded
                )
            })
        {
            return Ok(previous.observation.clone());
        }
        // Provider responses may finish out of order. A late sample cannot
        // replace a newer snapshot or close a restored interval before it opened.
        if state
            .current
            .as_ref()
            .is_some_and(|current| current.observation.observed_at > observed_at)
            || state
                .drift_intervals
                .iter()
                .any(|interval| interval.first_observed_at > observed_at)
        {
            return Err(GitHubStackCoordinatorErrorV1::InvalidProviderObservation);
        }
        let previous =
            registered_current.and_then(|published| published.observation.snapshot.as_ref());
        // Restored receipts retain the interval identity, not the provider
        // snapshot it was compared against. A first sample establishes a
        // baseline; it cannot prove that an open interval has ended. An
        // unavailable provider sample likewise provides no comparison. The
        // exception is an enabled provider response with no stack: only
        // EnabledWithoutStack can produce that pair, which proves absence
        // and can close intervals even without a retained snapshot.
        let comparison = match (previous, snapshot.as_ref()) {
            (Some(previous), Some(current)) => Some(stack_drift(previous, current)),
            (_, None)
                if matches!(&origin, ObservationOrigin::Provider(_))
                    && capability.state == GitHubStackCapabilityStateV1::Enabled =>
            {
                Some((Vec::new(), Vec::new()))
            }
            _ => None,
        };
        let (transitions, drift_observations) = match comparison {
            Some((transitions, drift_kinds)) => {
                let observations = transition::update_drift_intervals(
                    &mut state.drift_intervals,
                    &scope.scope_digest,
                    drift_kinds,
                    observed_at,
                )?;
                (transitions, observations)
            }
            None => (Vec::new(), Vec::new()),
        };
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
        state.current = Some(PublishedObservation {
            registration: origin.registration().cloned(),
            observation: observation.clone(),
        });
        Ok(observation)
    }
}

/// An abandoned owner must release admission and wake its joined waiters.
struct OwnedFlight<'a> {
    coordinator: &'a DaemonGitHubStackCoordinatorV1,
    key: PreflightKey,
    flight: Arc<InFlight>,
    _scope: Arc<RetainedScope>,
    circuit: Arc<ScopeCircuit>,
    now: UtcMicros,
    finished: bool,
}

impl OwnedFlight<'_> {
    fn finish(
        mut self,
        result: Result<NativeIntegrationPreflightOutcomeV1, StackCoordinatorErrorV1>,
    ) -> Result<NativeIntegrationPreflightOutcomeV1, StackCoordinatorErrorV1> {
        self.release(result)
    }

    fn release(
        &mut self,
        result: Result<NativeIntegrationPreflightOutcomeV1, StackCoordinatorErrorV1>,
    ) -> Result<NativeIntegrationPreflightOutcomeV1, StackCoordinatorErrorV1> {
        self.finished = true;
        let effect = circuit_effect(&result);
        let settled = (|| {
            let mut state = self.coordinator.preflights.lock().map_err(lock_error)?;
            state.in_flight.remove(&self.key);
            state.daemon_active = state.daemon_active.saturating_sub(1);
            if let Some(active) = state.repository_active.get_mut(&self.key.repository_id) {
                *active = active.saturating_sub(1);
                if *active == 0 {
                    state.repository_active.remove(&self.key.repository_id);
                }
            }
            // Settle before a successor can acquire the freed admission slot.
            match effect {
                CircuitEffect::Close => self
                    .circuit
                    .status
                    .lock()
                    .map(|mut status| {
                        *status = Circuit {
                            state: CircuitState::Closed,
                            failures: 0,
                        };
                    })
                    .map_err(lock_error),
                CircuitEffect::Fail => {
                    DaemonGitHubStackCoordinatorV1::fail_circuit(&self.circuit, self.now)
                }
                CircuitEffect::ReopenHalfOpen => {
                    DaemonGitHubStackCoordinatorV1::reopen_half_open(&self.circuit, self.now)
                }
            }
        })();
        let mut result = settled.and(result);
        let mut published = match self.flight.result.lock() {
            Ok(published) => published,
            Err(error) => {
                result = Err(invalid(&error));
                error.into_inner()
            }
        };
        *published = Some(result.clone());
        drop(published);
        self.flight.complete.notify_waiters();
        result
    }
}

impl Drop for OwnedFlight<'_> {
    fn drop(&mut self) {
        if !self.finished
            && let Err(error) = self.release(Err(StackCoordinatorErrorV1::Unavailable))
        {
            tracing::debug!(?error, "GitHub stack preflight owner was abandoned");
        }
    }
}

enum CircuitEffect {
    Close,
    Fail,
    ReopenHalfOpen,
}

fn circuit_effect(
    result: &Result<NativeIntegrationPreflightOutcomeV1, StackCoordinatorErrorV1>,
) -> CircuitEffect {
    match result {
        Ok(NativeIntegrationPreflightOutcomeV1::Preview(_)) => CircuitEffect::Close,
        Ok(
            NativeIntegrationPreflightOutcomeV1::Partial
            | NativeIntegrationPreflightOutcomeV1::DurabilityUncertain
            | NativeIntegrationPreflightOutcomeV1::ResetRequired
            | NativeIntegrationPreflightOutcomeV1::Cancelled
            | NativeIntegrationPreflightOutcomeV1::Unavailable,
        )
        | Err(
            StackCoordinatorErrorV1::Cancelled
            | StackCoordinatorErrorV1::Unavailable
            | StackCoordinatorErrorV1::Saturated,
        ) => CircuitEffect::Fail,
        Ok(
            NativeIntegrationPreflightOutcomeV1::Stale
            | NativeIntegrationPreflightOutcomeV1::Denied,
        )
        | Err(
            StackCoordinatorErrorV1::Denied
            | StackCoordinatorErrorV1::Stale
            | StackCoordinatorErrorV1::Invalid(_),
        ) => CircuitEffect::ReopenHalfOpen,
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

fn invalid(error: impl std::fmt::Display) -> StackCoordinatorErrorV1 {
    StackCoordinatorErrorV1::Invalid(error.to_string())
}
fn invalid_circuit(_error: StackCoordinatorErrorV1) -> GitHubStackCoordinatorErrorV1 {
    GitHubStackCoordinatorErrorV1::InvalidCircuitPolicy
}
fn lock_error<T>(error: std::sync::PoisonError<T>) -> StackCoordinatorErrorV1 {
    invalid(error)
}
