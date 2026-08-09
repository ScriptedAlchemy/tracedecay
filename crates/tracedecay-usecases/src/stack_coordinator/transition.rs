use serde::{Deserialize, Serialize};
use tracedecay_application::{
    CancellationSignal, NativeIntegrationPreflightOutcomeV1, NativeIntegrationPreflightRequestV1,
    ResolvedScope,
};
use tracedecay_domain::{
    ActorId, BranchStackRevisionId, CoverageStateV1, DurationBucketV1, GitHubStackSnapshotV1,
    IntervalStateV1, ManifestDigest, RepositoryId, StackDeliveryWatermarkId, StackDriftKindV1,
    StackSignalId, UtcMicros, WorkStackDriftObservedV1, canonical_sha256,
};

use super::{
    GitHubStackCoordinatorErrorV1, GitHubStackDriftObservationV1, StackCoordinatorErrorV1,
};

pub const MAX_REPOSITORY_PREFLIGHTS: usize = 4;
pub const MAX_DAEMON_PREFLIGHTS: usize = 16;
pub const MAX_BATCH_RECIPIENTS: usize = 64;
pub const MAX_BATCH_SIGNALS: usize = 128;
pub const DEDUPE_TTL_MICROS: i64 = 300_000_000;

#[derive(Clone, Debug)]
pub(super) struct DriftInterval {
    pub(super) kind: StackDriftKindV1,
    pub(super) trace_id: ManifestDigest,
    pub(super) first_observed_at: UtcMicros,
}

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
    pub(super) const fn debounce_micros(self) -> i64 {
        match self {
            Self::DependencyReady | Self::PotentialConflict => 250_000,
            Self::StackTipDrift | Self::PullRequestDrift | Self::CiEvaluatedCommitDrift => {
                1_000_000
            }
            _ => 0,
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
pub struct StackSignalDraftV1 {
    pub stack_revision_id: BranchStackRevisionId,
    pub stack_revision_digest: ManifestDigest,
    pub kind: StackSignalKindV1,
    pub state_digest: ManifestDigest,
    pub github_stack_digest: Option<ManifestDigest>,
    pub observed_at: UtcMicros,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StackSignalV1 {
    pub signal_id: StackSignalId,
    pub scope_digest: ManifestDigest,
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
    pub fn seal(
        scope: &ResolvedScope,
        draft: StackSignalDraftV1,
    ) -> Result<Self, StackCoordinatorErrorV1> {
        let (signal_id, watermark_id) = Self::derive_identities(scope, &draft)?;
        let signal = Self {
            signal_id,
            scope_digest: scope.scope_digest.clone(),
            repository_id: scope.repository_id.clone(),
            stack_revision_id: draft.stack_revision_id,
            stack_revision_digest: draft.stack_revision_digest,
            kind: draft.kind,
            state_digest: draft.state_digest,
            github_stack_digest: draft.github_stack_digest,
            observed_at: draft.observed_at,
            watermark_id,
        };
        signal.validate_fields(scope)?;
        Ok(signal)
    }

    pub fn validate(&self, scope: &ResolvedScope) -> Result<(), StackCoordinatorErrorV1> {
        self.validate_fields(scope)?;
        let draft = StackSignalDraftV1 {
            stack_revision_id: self.stack_revision_id.clone(),
            stack_revision_digest: self.stack_revision_digest.clone(),
            kind: self.kind,
            state_digest: self.state_digest.clone(),
            github_stack_digest: self.github_stack_digest.clone(),
            observed_at: self.observed_at,
        };
        let (signal_id, watermark_id) = Self::derive_identities(scope, &draft)?;
        if signal_id != self.signal_id || watermark_id != self.watermark_id {
            return Err(StackCoordinatorErrorV1::Invalid(
                "stack signal identity digest mismatch".to_owned(),
            ));
        }
        Ok(())
    }

    fn validate_fields(&self, scope: &ResolvedScope) -> Result<(), StackCoordinatorErrorV1> {
        scope.validate().map_err(invalid)?;
        self.signal_id.validate().map_err(invalid)?;
        self.repository_id.validate().map_err(invalid)?;
        self.stack_revision_id.validate().map_err(invalid)?;
        self.stack_revision_digest.validate().map_err(invalid)?;
        self.state_digest.validate().map_err(invalid)?;
        self.github_stack_digest
            .as_ref()
            .map_or(Ok(()), ManifestDigest::validate)
            .map_err(invalid)?;
        self.watermark_id.validate().map_err(invalid)?;
        if self.scope_digest != scope.scope_digest || self.repository_id != scope.repository_id {
            return Err(StackCoordinatorErrorV1::Stale);
        }
        Ok(())
    }

    fn derive_identities(
        scope: &ResolvedScope,
        draft: &StackSignalDraftV1,
    ) -> Result<(StackSignalId, StackDeliveryWatermarkId), StackCoordinatorErrorV1> {
        scope.validate().map_err(invalid)?;
        draft.stack_revision_id.validate().map_err(invalid)?;
        draft.stack_revision_digest.validate().map_err(invalid)?;
        draft.state_digest.validate().map_err(invalid)?;
        draft
            .github_stack_digest
            .as_ref()
            .map_or(Ok(()), ManifestDigest::validate)
            .map_err(invalid)?;
        let signal_digest = canonical_sha256(&(
            "tracedecay.stack-signal.v1",
            &scope.scope_digest,
            &draft.stack_revision_id,
            &draft.stack_revision_digest,
            draft.kind,
            &draft.state_digest,
            &draft.github_stack_digest,
            draft.observed_at,
        ))
        .map_err(invalid)?;
        let watermark_digest = canonical_sha256(&(
            "tracedecay.stack-delivery-watermark.v1",
            &scope.scope_digest,
            &draft.stack_revision_id,
            &draft.stack_revision_digest,
        ))
        .map_err(invalid)?;
        let signal_id = StackSignalId::new(format!(
            "signal.stack.{}",
            signal_digest.as_str().replace(':', ".")
        ))
        .map_err(invalid)?;
        let watermark_id = StackDeliveryWatermarkId::new(format!(
            "watermark.stack.{}",
            watermark_digest.as_str().replace(':', ".")
        ))
        .map_err(invalid)?;
        Ok((signal_id, watermark_id))
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
    ) -> Result<(), StackCoordinatorErrorV1>;
    fn pending_deliveries(
        &self,
    ) -> Result<Vec<(StackPendingDeliveryV1, StackSignalV1)>, StackCoordinatorErrorV1>;
    fn acknowledge(
        &self,
        watermark_id: &StackDeliveryWatermarkId,
        deliveries: &[StackPendingDeliveryV1],
    ) -> Result<(), StackCoordinatorErrorV1>;
    fn signal(
        &self,
        signal_id: &StackSignalId,
    ) -> Result<Option<StackSignalV1>, StackCoordinatorErrorV1>;
    fn record_authorization_loss(
        &self,
        signal: &StackSignalV1,
        recipient: &ActorId,
        outcome: StackDeliveryAuthorizationV1,
    ) -> Result<(), StackCoordinatorErrorV1>;
}

pub trait StackDeliveryAuthorizationPort: Send + Sync {
    fn select_recipients(
        &self,
        scope: &ResolvedScope,
        signal: &StackSignalV1,
    ) -> Result<Vec<ActorId>, StackCoordinatorErrorV1>;

    fn authorize(
        &self,
        recipient: &ActorId,
        signal: &StackSignalV1,
    ) -> StackDeliveryAuthorizationV1;
}

pub trait StackDeliveryPort: Send + Sync {
    fn deliver(&self, batch: &StackDeliveryBatchV1) -> Result<(), StackCoordinatorErrorV1>;
}

pub trait OptionalStackPreflightPort: Send + Sync {
    fn preflight(
        &self,
        request: &NativeIntegrationPreflightRequestV1,
        cancellation: &CancellationSignal,
    ) -> Result<NativeIntegrationPreflightOutcomeV1, StackCoordinatorErrorV1>;
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StackCircuitPolicyV1 {
    pub revision: u64,
    pub policy_digest: ManifestDigest,
    pub failure_threshold: u32,
    pub open_micros: i64,
}

impl StackCircuitPolicyV1 {
    pub fn seal(mut self) -> Result<Self, StackCoordinatorErrorV1> {
        if self.revision == 0 || self.failure_threshold == 0 || self.open_micros <= 0 {
            return Err(StackCoordinatorErrorV1::Invalid(
                "invalid stack circuit policy".to_owned(),
            ));
        }
        self.policy_digest = self.compute_digest()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), StackCoordinatorErrorV1> {
        self.policy_digest.validate().map_err(invalid)?;
        if self.clone().seal()?.policy_digest != self.policy_digest {
            return Err(StackCoordinatorErrorV1::Invalid(
                "stack circuit policy digest mismatch".to_owned(),
            ));
        }
        Ok(())
    }

    fn compute_digest(&self) -> Result<ManifestDigest, StackCoordinatorErrorV1> {
        canonical_sha256(&(
            "tracedecay.stack-circuit-policy.v1",
            self.revision,
            self.failure_threshold,
            self.open_micros,
        ))
        .map_err(invalid)
    }
}

fn invalid(error: impl std::fmt::Display) -> StackCoordinatorErrorV1 {
    StackCoordinatorErrorV1::Invalid(error.to_string())
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

pub(super) fn stack_drift(
    previous: Option<&GitHubStackSnapshotV1>,
    current: Option<&GitHubStackSnapshotV1>,
) -> (Vec<StackSignalKindV1>, Vec<StackDriftKindV1>) {
    let (Some(previous), Some(current)) = (previous, current) else {
        return (Vec::new(), Vec::new());
    };
    let mut signals = Vec::new();
    let mut drift = Vec::new();
    if previous.final_target_ref_id != current.final_target_ref_id {
        push_unique(&mut drift, StackDriftKindV1::Retargeted);
        push_unique(&mut signals, StackSignalKindV1::StackTipDrift);
    }
    if previous.final_target_commit_id != current.final_target_commit_id {
        push_unique(&mut drift, StackDriftKindV1::BaseAdvanced);
        push_unique(&mut signals, StackSignalKindV1::StackTipDrift);
    }
    for before in &previous.layers {
        let Some(layer) = current.layers.iter().find(|candidate| {
            candidate.pull_request.pull_request_id == before.pull_request.pull_request_id
        }) else {
            push_unique(&mut drift, StackDriftKindV1::Superseded);
            push_unique(&mut signals, StackSignalKindV1::PullRequestDrift);
            continue;
        };
        if before.base_ref_id != layer.base_ref_id || before.head_ref_id != layer.head_ref_id {
            push_unique(&mut drift, StackDriftKindV1::Retargeted);
            push_unique(&mut signals, StackSignalKindV1::PullRequestDrift);
        }
        if before.pull_request.head_commit_id != layer.pull_request.head_commit_id {
            push_unique(&mut drift, StackDriftKindV1::HeadAdvanced);
            push_unique(&mut signals, StackSignalKindV1::PullRequestDrift);
        }
        if before.pull_request.base_commit_id != layer.pull_request.base_commit_id {
            push_unique(&mut drift, StackDriftKindV1::BaseAdvanced);
            push_unique(&mut signals, StackSignalKindV1::PullRequestDrift);
        }
        if before.pull_request.merge_base_commit_id != layer.pull_request.merge_base_commit_id {
            push_unique(&mut drift, StackDriftKindV1::MergeBaseChanged);
            push_unique(&mut signals, StackSignalKindV1::PullRequestDrift);
        }
        if before.ci_digest != layer.ci_digest {
            push_unique(&mut signals, StackSignalKindV1::CiEvaluatedCommitDrift);
        }
    }
    (signals, drift)
}

pub(super) fn drift_observation(
    interval: &DriftInterval,
    state: IntervalStateV1,
    observed_at: UtcMicros,
) -> GitHubStackDriftObservationV1 {
    let age = observed_at.0.saturating_sub(interval.first_observed_at.0);
    GitHubStackDriftObservationV1 {
        trace_id: interval.trace_id.clone(),
        observed_at,
        drift: WorkStackDriftObservedV1 {
            kind: interval.kind,
            state,
            first_observed_micros: interval.first_observed_at.0,
            terminal_micros: (state == IntervalStateV1::Closed).then_some(observed_at.0),
            age_bucket: duration_bucket(age),
            coverage: CoverageStateV1::Known,
        },
    }
}

pub(super) fn validate_drift_observation(
    scope_digest: &ManifestDigest,
    observation: &GitHubStackDriftObservationV1,
) -> Result<(), GitHubStackCoordinatorErrorV1> {
    observation
        .drift
        .validate()
        .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidProviderObservation)?;
    let first = UtcMicros(observation.drift.first_observed_micros);
    if observation.observed_at.0 < first.0 {
        return Err(GitHubStackCoordinatorErrorV1::InvalidProviderObservation);
    }
    if drift_trace_id(scope_digest, observation.drift.kind, first)? != observation.trace_id {
        return Err(GitHubStackCoordinatorErrorV1::InvalidProviderObservation);
    }
    let terminal = match observation.drift.state {
        IntervalStateV1::Open if observation.drift.terminal_micros.is_none() => {
            observation.observed_at.0
        }
        IntervalStateV1::Closed
            if observation.drift.terminal_micros == Some(observation.observed_at.0) =>
        {
            observation.observed_at.0
        }
        _ => return Err(GitHubStackCoordinatorErrorV1::InvalidProviderObservation),
    };
    if observation.drift.age_bucket
        != duration_bucket(terminal.saturating_sub(observation.drift.first_observed_micros))
    {
        return Err(GitHubStackCoordinatorErrorV1::InvalidProviderObservation);
    }
    Ok(())
}

fn duration_bucket(age_micros: i64) -> DurationBucketV1 {
    if age_micros < 60_000_000 {
        DurationBucketV1::Under1m
    } else if age_micros < 300_000_000 {
        DurationBucketV1::From1mTo5m
    } else if age_micros < 900_000_000 {
        DurationBucketV1::From5mTo15m
    } else if age_micros < 3_600_000_000 {
        DurationBucketV1::From15mTo1h
    } else if age_micros < 14_400_000_000 {
        DurationBucketV1::From1hTo4h
    } else if age_micros < 86_400_000_000 {
        DurationBucketV1::From4hTo24h
    } else if age_micros < 604_800_000_000 {
        DurationBucketV1::From1dTo7d
    } else {
        DurationBucketV1::Over7d
    }
}

pub(super) fn drift_trace_id(
    scope_digest: &ManifestDigest,
    kind: StackDriftKindV1,
    first_observed_at: UtcMicros,
) -> Result<ManifestDigest, super::GitHubStackCoordinatorErrorV1> {
    canonical_sha256(&(
        "tracedecay.github-stack.drift-trace.v1",
        scope_digest,
        kind,
        first_observed_at,
    ))
    .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidProviderObservation)
}

pub(super) fn update_drift_intervals(
    intervals: &Mutex<BTreeMap<ManifestDigest, Vec<DriftInterval>>>,
    scope_digest: &ManifestDigest,
    current_kinds: Vec<StackDriftKindV1>,
    observed_at: UtcMicros,
) -> Result<Vec<GitHubStackDriftObservationV1>, GitHubStackCoordinatorErrorV1> {
    let mut by_scope = intervals
        .lock()
        .map_err(|_| GitHubStackCoordinatorErrorV1::Poisoned)?;
    let active = by_scope.entry(scope_digest.clone()).or_default();
    let mut observations = Vec::new();
    for interval in active
        .iter()
        .filter(|interval| !current_kinds.contains(&interval.kind))
    {
        observations.push(drift_observation(
            interval,
            IntervalStateV1::Closed,
            observed_at,
        ));
    }
    active.retain(|interval| current_kinds.contains(&interval.kind));
    for kind in current_kinds {
        let index = active.iter().position(|interval| interval.kind == kind);
        if index.is_none() {
            active.push(DriftInterval {
                kind,
                trace_id: drift_trace_id(scope_digest, kind, observed_at)?,
                first_observed_at: observed_at,
            });
        }
        observations.push(drift_observation(
            &active[index.unwrap_or(active.len() - 1)],
            IntervalStateV1::Open,
            observed_at,
        ));
    }
    Ok(observations)
}

pub(super) fn restore_open_drift_interval(
    intervals: &Mutex<BTreeMap<ManifestDigest, Vec<DriftInterval>>>,
    scope: &ResolvedScope,
    observation: &GitHubStackDriftObservationV1,
) -> Result<(), GitHubStackCoordinatorErrorV1> {
    scope
        .validate()
        .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidScope)?;
    validate_drift_observation(&scope.scope_digest, observation)?;
    if observation.drift.state != IntervalStateV1::Open {
        return Err(GitHubStackCoordinatorErrorV1::InvalidProviderObservation);
    }
    let first_observed_at = UtcMicros(observation.drift.first_observed_micros);
    if drift_trace_id(
        &scope.scope_digest,
        observation.drift.kind,
        first_observed_at,
    )? != observation.trace_id
    {
        return Err(GitHubStackCoordinatorErrorV1::InvalidProviderObservation);
    }
    let interval = DriftInterval {
        kind: observation.drift.kind,
        trace_id: observation.trace_id.clone(),
        first_observed_at,
    };
    let mut by_scope = intervals
        .lock()
        .map_err(|_| GitHubStackCoordinatorErrorV1::Poisoned)?;
    let active = by_scope.entry(scope.scope_digest.clone()).or_default();
    if let Some(existing) = active
        .iter_mut()
        .find(|existing| existing.kind == interval.kind)
    {
        *existing = interval;
    } else {
        active.push(interval);
    }
    Ok(())
}

fn push_unique<T: PartialEq>(values: &mut Vec<T>, value: T) {
    if !values.contains(&value) {
        values.push(value);
    }
}
use std::collections::BTreeMap;
use std::sync::Mutex;
