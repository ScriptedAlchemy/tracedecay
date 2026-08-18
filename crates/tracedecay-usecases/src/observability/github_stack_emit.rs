//! Read-only GitHub stacked-PR capability receipts.
//!
//! The daemon-local stack coordinator is the capability authority. This owner
//! verifies that a coordinator observation belongs to its exact project and
//! repository mount, then carries the coordinator's four-state outcome and
//! immutable evidence into the bounded Observatory outbox.

use std::collections::BTreeMap;

use tracedecay_application::{
    MAX_EXECUTION_TOPOLOGY_EVENTS_V1, ObservabilityHorizonV1, ObservabilityQueryPort,
    ObservabilityQueryV1, context::ResolvedScope,
};
use tracedecay_domain::{
    CoverageStateV1, GitHubStackCapabilityObservedV1, GitHubStackCapabilityStateV1,
    GitHubStackCapabilityV1, GitTopologyAnchorTargetV1, IntervalStateV1, ManifestDigest,
    ObservabilityEnvelopeV1, ObservabilityPayloadV1, ObservabilityTerminalResultV1,
    ObservationScopeV1, UtcMicros, canonical_sha256,
    configuration::{ReviewTopologyKindV1, WorkTopologyPolicyV1},
    derive_git_topology_anchor_id,
};
use tracedecay_global_db::RegisteredGlobalDb;

use crate::advisory::github_runtime::GitHubCiRepositoryTargetV1;
use crate::stack_coordinator::{
    GitHubStackDriftObservationV1, GitHubStackObservationV1, GitHubStackProviderSourceBindingV1,
};

use super::{
    BoundedObservabilityProducerV1, ExecutionOwnerFactInputV1, ObservabilityEmissionOutcomeV1,
    ObservabilityProducerIdentityV1, execution_owner_fact_envelope,
};

const EVENT_KIND: &str = "work.github_stack_capability.observed.v1";
const DRIFT_EVENT_KIND: &str = "work.stack_drift.observed.v1";
const TELEMETRY_DROP_EVENT_KIND: &str = "telemetry.drop.observed.v1";
const OPERATION: &str = "probe_github_stacked_pull_request_capability";
const DRIFT_OPERATION: &str = "observe_github_stack_drift";
const PROBE_REVISION: &str = "github-stack-coordinator.v1";
const DRIFT_RETENTION_MICROS: i64 = 30 * 86_400_000_000;

/// Exact mount failure. Malformed scope or policy never becomes a misleading
/// product observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitHubStackProbeOwnerMountErrorV1 {
    InvalidScope,
    InvalidTopologyPolicy,
    InvalidGitHubRepository,
}

/// Why an owner did not offer a coordinator-backed capability fact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitHubStackCapabilityObservationUnavailableV1 {
    CoordinatorScopeMismatch,
    CoordinatorEvidenceInvalid,
    ProducerUnmounted,
    ProducerScopeMismatch,
    ProducerAdmissionUnavailable,
    DatabaseScopeMismatch,
    OwnerReceiptInvalid,
}

/// Result of offering the receipt through the only durable producer/outbox.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GitHubStackCapabilityObservationResultV1 {
    Enqueued,
    DroppedAtCapacity,
    Unavailable {
        event_kind: &'static str,
        reason: GitHubStackCapabilityObservationUnavailableV1,
    },
}

/// Why an exact coordinator drift interval could not enter the canonical
/// bounded producer. The whole coordinator observation is rejected before
/// enqueue when any interval is invalid, so one transition cannot be partly
/// recorded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitHubStackDriftObservationUnavailableV1 {
    CoordinatorScopeMismatch,
    CoordinatorEvidenceInvalid,
    ProducerUnmounted,
    ProducerScopeMismatch,
    ProducerAdmissionUnavailable,
    DatabaseScopeMismatch,
    OwnerReceiptInvalid,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GitHubStackDriftObservationResultV1 {
    Emitted {
        enqueued: usize,
        dropped: usize,
    },
    Unavailable {
        event_kind: &'static str,
        reason: GitHubStackDriftObservationUnavailableV1,
    },
}

/// Fail-closed reasons for restoring coordinator intervals from the retained
/// canonical observation authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitHubStackDriftRecoveryErrorV1 {
    InvalidScope,
    DatabaseScopeMismatch,
    ObservationStoreUnavailable,
    IncompleteCoverage,
    InvalidEvidence,
}

/// Project-scoped Plan 37 receipt owner. Observation time and provider state
/// come only from the coordinator evidence supplied to emission.
#[derive(Clone, Debug)]
pub struct GitHubStackProbeOwnerV1 {
    scope: ResolvedScope,
    github_source_anchor: tracedecay_domain::ManifestDigest,
    standard_git_fallback_available: bool,
}

impl GitHubStackProbeOwnerV1 {
    /// Mounts an exact project/ref owner after the production composition root
    /// has verified that the project is a GitHub remote. This constructor does
    /// not accept a token and cannot perform a write.
    pub fn mount(
        scope: ResolvedScope,
        topology_policy: WorkTopologyPolicyV1,
        github_owner: &str,
        github_repository: &str,
        native_git_fallback_mounted: bool,
    ) -> Result<Self, GitHubStackProbeOwnerMountErrorV1> {
        scope
            .validate()
            .map_err(|_| GitHubStackProbeOwnerMountErrorV1::InvalidScope)?;
        topology_policy
            .validate()
            .map_err(|_| GitHubStackProbeOwnerMountErrorV1::InvalidTopologyPolicy)?;
        let source = GitHubCiRepositoryTargetV1 {
            owner: github_owner.to_owned(),
            repository: github_repository.to_owned(),
        };
        if !source.validate() {
            return Err(GitHubStackProbeOwnerMountErrorV1::InvalidGitHubRepository);
        }
        let github_source_anchor = canonical_sha256(&(
            "tracedecay.github-stack.source-anchor.v1",
            &source.owner,
            &source.repository,
        ))
        .map_err(|_| GitHubStackProbeOwnerMountErrorV1::InvalidGitHubRepository)?;
        Ok(Self {
            scope,
            github_source_anchor,
            standard_git_fallback_available: native_git_fallback_mounted
                && topology_policy
                    .review_topology
                    .allowed
                    .contains(&ReviewTopologyKindV1::StandardPullRequests),
        })
    }

    fn product_observation(
        &self,
        coordinator: &GitHubStackObservationV1,
    ) -> Result<GitHubStackCapabilityObservedV1, GitHubStackCapabilityObservationUnavailableV1>
    {
        let (capability, coverage) = match coordinator.capability.state {
            GitHubStackCapabilityStateV1::Unavailable => (
                GitHubStackCapabilityV1::Unavailable,
                CoverageStateV1::Unknown,
            ),
            GitHubStackCapabilityStateV1::PrivatePreviewDisabled => (
                GitHubStackCapabilityV1::PrivatePreviewDisabled,
                CoverageStateV1::Known,
            ),
            GitHubStackCapabilityStateV1::Enabled => {
                (GitHubStackCapabilityV1::Enabled, CoverageStateV1::Known)
            }
            GitHubStackCapabilityStateV1::Degraded => {
                (GitHubStackCapabilityV1::Degraded, CoverageStateV1::Partial)
            }
        };
        Ok(GitHubStackCapabilityObservedV1 {
            capability,
            probe_revision: PROBE_REVISION.to_owned(),
            standard_git_fallback_available: self.standard_git_fallback_available,
            // This owner has no non-GitHub forge authority. Absence of one is
            // not inferred from the coordinator observation.
            other_forge_fallback_available: false,
            coverage,
        })
    }

    fn owner_transition_ref(
        &self,
        coordinator: &GitHubStackObservationV1,
    ) -> Result<String, GitHubStackCapabilityObservationUnavailableV1> {
        let receipt = canonical_sha256(&(
            "tracedecay.github-stack.coordinator-capability-receipt.v1",
            &self.scope.scope_digest,
            &self.github_source_anchor,
            &coordinator.capability_anchor_id,
            &coordinator.capability.content_digest,
            coordinator.snapshot_anchor_id.as_ref(),
            coordinator
                .snapshot
                .as_ref()
                .map(|snapshot| &snapshot.content_digest),
            coordinator.observed_at,
        ))
        .map_err(|_| GitHubStackCapabilityObservationUnavailableV1::OwnerReceiptInvalid)?;
        Ok(format!("github-stack-capability:{}", receipt.as_str()))
    }

    fn validate_coordinator_observation(
        &self,
        coordinator: &GitHubStackObservationV1,
        source_binding: &GitHubStackProviderSourceBindingV1,
    ) -> Result<(), GitHubStackCapabilityObservationUnavailableV1> {
        if coordinator.scope != self.scope
            || coordinator.capability.project_id != self.scope.project_id
            || coordinator.capability.repository_id != self.scope.repository_id
            || coordinator.capability.worktree_id != self.scope.worktree_id
        {
            return Err(GitHubStackCapabilityObservationUnavailableV1::CoordinatorScopeMismatch);
        }
        coordinator.capability.validate().map_err(|_| {
            GitHubStackCapabilityObservationUnavailableV1::CoordinatorEvidenceInvalid
        })?;
        source_binding.owner.validate().map_err(|_| {
            GitHubStackCapabilityObservationUnavailableV1::CoordinatorEvidenceInvalid
        })?;
        if source_binding.owner.project_id() != Some(&self.scope.project_id)
            || coordinator.capability.source_anchor_id != source_binding.capability_source_anchor_id
        {
            return Err(GitHubStackCapabilityObservationUnavailableV1::CoordinatorEvidenceInvalid);
        }
        if coordinator.capability.provider.as_str() != "provider.github" {
            return Err(GitHubStackCapabilityObservationUnavailableV1::CoordinatorEvidenceInvalid);
        }
        let anchor_owner = ObservationScopeV1::Project {
            project_id: self.scope.project_id.clone(),
        };
        let expected_capability_anchor = derive_git_topology_anchor_id(
            &anchor_owner,
            &GitTopologyAnchorTargetV1::GitHubStackCapability(coordinator.capability.clone()),
        )
        .map_err(|_| GitHubStackCapabilityObservationUnavailableV1::CoordinatorEvidenceInvalid)?;
        if coordinator.capability_anchor_id != expected_capability_anchor {
            return Err(GitHubStackCapabilityObservationUnavailableV1::CoordinatorEvidenceInvalid);
        }
        if coordinator.snapshot_anchor_id.is_some() != coordinator.snapshot.is_some()
            || source_binding.snapshot_source_anchor_id.is_some() != coordinator.snapshot.is_some()
            || (coordinator.snapshot.is_some()
                && coordinator.capability.state != GitHubStackCapabilityStateV1::Enabled)
        {
            return Err(GitHubStackCapabilityObservationUnavailableV1::CoordinatorEvidenceInvalid);
        }
        if let Some(snapshot) = &coordinator.snapshot {
            snapshot.validate().map_err(|_| {
                GitHubStackCapabilityObservationUnavailableV1::CoordinatorEvidenceInvalid
            })?;
            if snapshot.capability != coordinator.capability {
                return Err(
                    GitHubStackCapabilityObservationUnavailableV1::CoordinatorEvidenceInvalid,
                );
            }
            if Some(&snapshot.source_anchor_id) != source_binding.snapshot_source_anchor_id.as_ref()
            {
                return Err(
                    GitHubStackCapabilityObservationUnavailableV1::CoordinatorEvidenceInvalid,
                );
            }
            let expected_snapshot_anchor = derive_git_topology_anchor_id(
                &anchor_owner,
                &GitTopologyAnchorTargetV1::GitHubStackSnapshot(snapshot.clone()),
            )
            .map_err(|_| {
                GitHubStackCapabilityObservationUnavailableV1::CoordinatorEvidenceInvalid
            })?;
            if coordinator.snapshot_anchor_id.as_ref() != Some(&expected_snapshot_anchor) {
                return Err(
                    GitHubStackCapabilityObservationUnavailableV1::CoordinatorEvidenceInvalid,
                );
            }
        }
        Ok(())
    }
}

/// Offers a capability receipt only for a validated coordinator observation.
/// The bounded producer owns outbox claim and settlement off the project-open
/// path; capacity exhaustion is surfaced explicitly and carried by its normal
/// telemetry-drop evidence rather than delaying project admission.
pub fn record_github_stack_capability(
    db: &RegisteredGlobalDb,
    producer: Option<&BoundedObservabilityProducerV1>,
    owner: &GitHubStackProbeOwnerV1,
    source_binding: &GitHubStackProviderSourceBindingV1,
    coordinator: &GitHubStackObservationV1,
) -> GitHubStackCapabilityObservationResultV1 {
    let observation = match owner
        .validate_coordinator_observation(coordinator, source_binding)
        .and_then(|_| owner.product_observation(coordinator))
    {
        Ok(observation) => observation,
        Err(reason) => return unavailable(reason),
    };
    let Some(producer) = producer else {
        return unavailable(GitHubStackCapabilityObservationUnavailableV1::ProducerUnmounted);
    };
    let scope_ref = match db
        .binding()
        .shard_id
        .scope
        .project_id()
        .map(|project_id| project_id.as_str())
    {
        Some(project_id) if project_id == owner.scope.project_id.as_str() => project_id,
        Some(_) => {
            return unavailable(
                GitHubStackCapabilityObservationUnavailableV1::DatabaseScopeMismatch,
            );
        }
        None => {
            return unavailable(
                GitHubStackCapabilityObservationUnavailableV1::DatabaseScopeMismatch,
            );
        }
    };
    if producer.identity().authorized_scope_ref != scope_ref {
        return unavailable(GitHubStackCapabilityObservationUnavailableV1::ProducerScopeMismatch);
    }
    let owner_transition_ref = match owner.owner_transition_ref(coordinator) {
        Ok(reference) => reference,
        Err(reason) => return unavailable(reason),
    };
    let envelope = match execution_owner_fact_envelope(
        producer.identity(),
        scope_ref,
        ExecutionOwnerFactInputV1 {
            owner_transition_ref: &owner_transition_ref,
            operation: OPERATION,
            event_time: coordinator.observed_at,
            valid_from: Some(coordinator.observed_at),
            valid_until: Some(coordinator.observed_at),
            terminal_result: Some(ObservabilityTerminalResultV1::Succeeded),
            coverage: observation.coverage,
            payload: ObservabilityPayloadV1::GitHubStackCapability(observation),
        },
    ) {
        Ok(envelope) => envelope,
        Err(_) => {
            return unavailable(GitHubStackCapabilityObservationUnavailableV1::OwnerReceiptInvalid);
        }
    };
    match producer.try_emit_owner_fact(envelope) {
        Ok(ObservabilityEmissionOutcomeV1::Enqueued) => {
            GitHubStackCapabilityObservationResultV1::Enqueued
        }
        Ok(ObservabilityEmissionOutcomeV1::DroppedAtCapacity) => {
            GitHubStackCapabilityObservationResultV1::DroppedAtCapacity
        }
        Err(_) => {
            unavailable(GitHubStackCapabilityObservationUnavailableV1::ProducerAdmissionUnavailable)
        }
    }
}

/// Offers every exact drift interval returned by the mounted coordinator to
/// the one bounded Observatory producer. The interval's canonical trace is
/// the owner transition reference, so open and closed observations retain one
/// correction identity while the generic envelope keeps that identity opaque.
pub fn record_github_stack_drifts(
    db: &RegisteredGlobalDb,
    producer: Option<&BoundedObservabilityProducerV1>,
    owner: &GitHubStackProbeOwnerV1,
    source_binding: &GitHubStackProviderSourceBindingV1,
    coordinator: &GitHubStackObservationV1,
) -> GitHubStackDriftObservationResultV1 {
    if coordinator.scope != owner.scope {
        return drift_unavailable(
            GitHubStackDriftObservationUnavailableV1::CoordinatorScopeMismatch,
        );
    }
    if owner
        .validate_coordinator_observation(coordinator, source_binding)
        .is_err()
    {
        return drift_unavailable(
            GitHubStackDriftObservationUnavailableV1::CoordinatorEvidenceInvalid,
        );
    }
    let Some(producer) = producer else {
        return drift_unavailable(GitHubStackDriftObservationUnavailableV1::ProducerUnmounted);
    };
    let scope_ref = match db.binding().shard_id.scope.project_id() {
        Some(project_id) if project_id.as_str() == owner.scope.project_id.as_str() => {
            project_id.as_str()
        }
        _ => {
            return drift_unavailable(
                GitHubStackDriftObservationUnavailableV1::DatabaseScopeMismatch,
            );
        }
    };
    if producer.identity().authorized_scope_ref != scope_ref {
        return drift_unavailable(GitHubStackDriftObservationUnavailableV1::ProducerScopeMismatch);
    }
    let envelopes = match coordinator
        .drift_observations
        .iter()
        .map(|observation| {
            stack_drift_envelope(
                producer.identity(),
                scope_ref,
                &owner.scope.scope_digest,
                observation,
            )
        })
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(envelopes) => envelopes,
        Err(_) => {
            return drift_unavailable(
                GitHubStackDriftObservationUnavailableV1::OwnerReceiptInvalid,
            );
        }
    };
    let outcomes = match producer.try_emit_owner_facts(envelopes) {
        Ok(outcomes) => outcomes,
        Err(_) => {
            return drift_unavailable(
                GitHubStackDriftObservationUnavailableV1::ProducerAdmissionUnavailable,
            );
        }
    };
    let mut enqueued = 0usize;
    let mut dropped = 0usize;
    for outcome in outcomes {
        match outcome {
            ObservabilityEmissionOutcomeV1::Enqueued => {
                enqueued = enqueued.saturating_add(1);
            }
            ObservabilityEmissionOutcomeV1::DroppedAtCapacity => {
                dropped = dropped.saturating_add(1);
            }
        }
    }
    GitHubStackDriftObservationResultV1::Emitted { enqueued, dropped }
}

/// Reads the latest retained drift observations and returns only intervals
/// that remain provably open. A closed interval is monotone: even a later
/// replay of its open fact cannot reopen it during daemon restart.
pub async fn recover_open_github_stack_drifts(
    db: &RegisteredGlobalDb,
    scope: &ResolvedScope,
    now: UtcMicros,
) -> Result<Vec<GitHubStackDriftObservationV1>, GitHubStackDriftRecoveryErrorV1> {
    scope
        .validate()
        .map_err(|_| GitHubStackDriftRecoveryErrorV1::InvalidScope)?;
    if db.binding().shard_id.scope.project_id() != Some(&scope.project_id) {
        return Err(GitHubStackDriftRecoveryErrorV1::DatabaseScopeMismatch);
    }
    let page = super::RegisteredObservabilityPortV1::new(db)
        .query(ObservabilityQueryV1 {
            authorized_scope_ref: scope.project_id.as_str().to_owned(),
            event_kinds: vec![
                DRIFT_EVENT_KIND.to_owned(),
                TELEMETRY_DROP_EVENT_KIND.to_owned(),
            ],
            horizon: ObservabilityHorizonV1 {
                since_micros: now.0.saturating_sub(DRIFT_RETENTION_MICROS),
                until_micros: now.0.saturating_add(1),
            },
            after_watermark: None,
            limit: MAX_EXECUTION_TOPOLOGY_EVENTS_V1,
        })
        .await
        .map_err(|_| GitHubStackDriftRecoveryErrorV1::ObservationStoreUnavailable)?;
    if page.coverage != CoverageStateV1::Known || page.next_watermark.is_some() {
        return Err(GitHubStackDriftRecoveryErrorV1::IncompleteCoverage);
    }
    let mut latest = BTreeMap::<ManifestDigest, GitHubStackDriftObservationV1>::new();
    for event in page.events {
        let drift = match &event.payload {
            ObservabilityPayloadV1::WorkStackDrift(drift) if event.dropped_count == 0 => drift,
            ObservabilityPayloadV1::TelemetryDrop(_) => {
                return Err(GitHubStackDriftRecoveryErrorV1::IncompleteCoverage);
            }
            _ => return Err(GitHubStackDriftRecoveryErrorV1::InvalidEvidence),
        };
        let observation = GitHubStackDriftObservationV1::from_persisted(
            &scope.scope_digest,
            UtcMicros(event.event_time_micros),
            drift.clone(),
        )
        .map_err(|_| GitHubStackDriftRecoveryErrorV1::InvalidEvidence)?;
        let identity = ObservabilityProducerIdentityV1 {
            authorized_scope_ref: event.scope_ref.clone(),
            process_boot_id: event.process_boot_id.clone(),
            producer_revision: event.producer_revision.clone(),
            configuration_revision: event.configuration_revision.clone(),
            policy_revision: event.policy_revision.clone(),
        };
        let expected = stack_drift_envelope(
            &identity,
            scope.project_id.as_str(),
            &scope.scope_digest,
            &observation,
        )
        .map_err(|_| GitHubStackDriftRecoveryErrorV1::InvalidEvidence)?;
        if !same_drift_owner_fact(&event, &expected) {
            return Err(GitHubStackDriftRecoveryErrorV1::InvalidEvidence);
        }
        match latest.get_mut(&observation.trace_id) {
            None => {
                latest.insert(observation.trace_id.clone(), observation);
            }
            Some(current) if current.drift.state == IntervalStateV1::Closed => {}
            Some(current) if observation.drift.state == IntervalStateV1::Closed => {
                *current = observation;
            }
            Some(current) if observation.observed_at > current.observed_at => {
                *current = observation;
            }
            Some(_) => {}
        }
    }
    let mut open_by_kind = BTreeMap::new();
    for observation in latest.into_values().filter(|observation| {
        observation.drift.state == IntervalStateV1::Open
            && observation.drift.coverage == CoverageStateV1::Known
    }) {
        if open_by_kind
            .insert(observation.drift.kind, observation)
            .is_some()
        {
            return Err(GitHubStackDriftRecoveryErrorV1::InvalidEvidence);
        }
    }
    Ok(open_by_kind.into_values().collect())
}

fn stack_drift_envelope(
    identity: &ObservabilityProducerIdentityV1,
    scope_ref: &str,
    scope_digest: &ManifestDigest,
    observation: &GitHubStackDriftObservationV1,
) -> Result<ObservabilityEnvelopeV1, &'static str> {
    observation
        .validate(scope_digest)
        .map_err(|_| "github_stack_drift_observation")?;
    let owner_transition_ref = format!("github-stack-drift:{}", observation.trace_id.as_str());
    execution_owner_fact_envelope(
        identity,
        scope_ref,
        ExecutionOwnerFactInputV1 {
            owner_transition_ref: &owner_transition_ref,
            operation: DRIFT_OPERATION,
            event_time: observation.observed_at,
            valid_from: Some(UtcMicros(observation.drift.first_observed_micros)),
            valid_until: observation.drift.terminal_micros.map(UtcMicros),
            terminal_result: None,
            coverage: observation.drift.coverage,
            payload: ObservabilityPayloadV1::WorkStackDrift(observation.drift.clone()),
        },
    )
}

fn same_drift_owner_fact(
    persisted: &ObservabilityEnvelopeV1,
    expected: &ObservabilityEnvelopeV1,
) -> bool {
    persisted.event_id == expected.event_id
        && persisted.event_kind == expected.event_kind
        && persisted.idempotency_key == expected.idempotency_key
        && persisted.trace_id == expected.trace_id
        && persisted.scope_ref == expected.scope_ref
        && persisted.capability == expected.capability
        && persisted.operation == expected.operation
        && persisted.event_time_micros == expected.event_time_micros
        && persisted.observation_time_micros == expected.observation_time_micros
        && persisted.valid_from_micros == expected.valid_from_micros
        && persisted.valid_until_micros == expected.valid_until_micros
        && persisted.coverage == expected.coverage
        && persisted.payload == expected.payload
}

const fn drift_unavailable(
    reason: GitHubStackDriftObservationUnavailableV1,
) -> GitHubStackDriftObservationResultV1 {
    GitHubStackDriftObservationResultV1::Unavailable {
        event_kind: DRIFT_EVENT_KIND,
        reason,
    }
}

const fn unavailable(
    reason: GitHubStackCapabilityObservationUnavailableV1,
) -> GitHubStackCapabilityObservationResultV1 {
    GitHubStackCapabilityObservationResultV1::Unavailable {
        event_kind: EVENT_KIND,
        reason,
    }
}
