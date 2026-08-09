//! Production execution-topology observations projected from owning receipts.
//!
//! This module deliberately accepts the bounded application result produced by
//! the native-integration owner, not caller fields and not a prebuilt Plan 26
//! payload. A durable preview or receipt can therefore emit the transition it
//! actually proves. Reads, unmounted owners, and results without an owner
//! receipt return typed unavailable and write nothing.

use tracedecay_application::{
    NativeIntegrationReceiptProjectionV1, NativeIntegrationSurfaceResultV1,
};
use tracedecay_domain::{
    CoverageStateV1, IntegrationOperationKindV1, IntegrationOwnerReceiptV1, IntegrationPhaseV1,
    IntegrationResultV1, IntegrationScopeClassV1, MechanicalIntegrationModeV1,
    NativeIntegrationPreviewDispositionV1, NativeIntegrationPreviewV1,
    NativeIntegrationTerminalOutcomeV1, NativeIntegrationUnavailabilityV1, ObservabilityEnvelopeV1,
    ObservabilityPayloadV1, ObservabilityRetentionClassV1, ObservabilityTerminalResultV1,
    UtcMicros, WorkIntegrationTransitionObservedV1, canonical_sha256,
};

use super::{
    BoundedObservabilityProducerV1, ObservabilityEmissionOutcomeV1, ObservabilityProducerIdentityV1,
};

const EVENT_KIND: &str = "work.integration.transition.observed.v1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionTopologyObservationUnavailableV1 {
    OwnerUnmounted,
    ProducerUnmounted,
    ProducerAdmissionUnavailable,
    OwnerEvidenceIncomplete,
    OwnerEvidenceInvalid,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeIntegrationObservationResultV1 {
    Emitted {
        enqueued: u8,
        dropped: u8,
    },
    Unavailable {
        event_kind: &'static str,
        reason: ExecutionTopologyObservationUnavailableV1,
    },
}

/// Persists transitions proved by the mounted Plan 36 owner. A preview proves
/// one dry-run terminal transition; a terminal apply receipt proves one apply
/// terminal and, only when committed, one native-integrated transition.
///
/// Emission never changes the already-decided integration result. The mounted
/// bounded producer owns sequencing, durable replay, and drop-carrier evidence.
/// Offering the completed owner facts is synchronous and bounded: storage and
/// replay never delay the product response.
pub fn record_native_integration_transition(
    scope_ref: &str,
    producer: Option<&BoundedObservabilityProducerV1>,
    surface_operation: &str,
    owner_mounted: bool,
    result: &NativeIntegrationSurfaceResultV1,
    owner_preview: Option<&NativeIntegrationPreviewV1>,
) -> NativeIntegrationObservationResultV1 {
    if !owner_mounted {
        return unavailable(ExecutionTopologyObservationUnavailableV1::OwnerUnmounted);
    }
    let Some(producer) = producer else {
        return unavailable(ExecutionTopologyObservationUnavailableV1::ProducerUnmounted);
    };
    let identity = producer.identity();
    if identity.authorized_scope_ref != scope_ref {
        return unavailable(ExecutionTopologyObservationUnavailableV1::OwnerEvidenceInvalid);
    }
    let envelopes = match native_integration_envelopes(
        identity,
        scope_ref,
        surface_operation,
        result,
        owner_preview,
    ) {
        Ok(envelopes) if !envelopes.is_empty() => envelopes,
        Ok(_) => {
            return unavailable(ExecutionTopologyObservationUnavailableV1::OwnerEvidenceIncomplete);
        }
        Err(_) => {
            return unavailable(ExecutionTopologyObservationUnavailableV1::OwnerEvidenceInvalid);
        }
    };
    let outcomes = match producer.try_emit_owner_facts(envelopes) {
        Ok(outcomes) => outcomes,
        Err(_) => {
            return unavailable(
                ExecutionTopologyObservationUnavailableV1::ProducerAdmissionUnavailable,
            );
        }
    };
    let mut enqueued = 0u8;
    let mut dropped = 0u8;
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
    NativeIntegrationObservationResultV1::Emitted { enqueued, dropped }
}

const fn unavailable(
    reason: ExecutionTopologyObservationUnavailableV1,
) -> NativeIntegrationObservationResultV1 {
    NativeIntegrationObservationResultV1::Unavailable {
        event_kind: EVENT_KIND,
        reason,
    }
}

fn native_integration_envelopes(
    identity: &ObservabilityProducerIdentityV1,
    scope_ref: &str,
    surface_operation: &str,
    result: &NativeIntegrationSurfaceResultV1,
    owner_preview: Option<&NativeIntegrationPreviewV1>,
) -> Result<Vec<ObservabilityEnvelopeV1>, &'static str> {
    let durable_preview = owner_preview.filter(|preview| {
        preview.validate().is_ok()
            && match result {
                NativeIntegrationSurfaceResultV1::Preview(projected) => {
                    preview.preview_id == projected.preview_id
                        && preview.preview_digest == projected.preview_digest
                }
                NativeIntegrationSurfaceResultV1::Receipt(receipt) => {
                    preview.preview_id == receipt.status.preview_id
                        && preview.preview_digest == receipt.status.preview_digest
                }
                _ => false,
            }
    });
    match result {
        NativeIntegrationSurfaceResultV1::Preview(preview)
            if surface_operation == "preflight_native_integration" =>
        {
            let (result, operation, terminal_result) = preview_disposition(&preview.disposition);
            let anchor = format!("native-preview:{}", preview.preview_id.as_str());
            let dependency_commits = u16::try_from(preview.ordered_commit_count)
                .map_err(|_| "native_integration_dependency_count")?;
            let coverage = preview_coverage(&preview.disposition);
            let observation = WorkIntegrationTransitionObservedV1 {
                phase: IntegrationPhaseV1::DryRunTerminal,
                result,
                operation,
                source_scope: IntegrationScopeClassV1::Repository,
                target_scope: IntegrationScopeClassV1::Repository,
                dependency_commits_eligible: dependency_commits,
                dependency_commits_observed: dependency_commits,
                required_checks_eligible: 0,
                required_checks_observed: 0,
                owner_receipt: IntegrationOwnerReceiptV1::NativeGitObservation,
                coverage,
                local_anchor_refs: vec![anchor.clone()],
            };
            let transition = transition_envelope(
                identity,
                scope_ref,
                &anchor,
                "preflight_native_integration",
                preview.created_at,
                terminal_result,
                observation,
            )?;
            Ok(vec![transition])
        }
        NativeIntegrationSurfaceResultV1::Receipt(receipt)
            if surface_operation == "apply_native_integration" =>
        {
            receipt_envelopes(identity, scope_ref, receipt, durable_preview)
        }
        // Status is a read and a replay of a previously emitted owner
        // transition. Snapshot/approval/worktree results do not prove one of
        // Plan 26's integration phases. Typed unavailability is safer than a
        // duplicate or inferred event.
        NativeIntegrationSurfaceResultV1::StackSnapshot(_)
        | NativeIntegrationSurfaceResultV1::Approval(_)
        | NativeIntegrationSurfaceResultV1::Status(_)
        | NativeIntegrationSurfaceResultV1::Cancellation(_)
        | NativeIntegrationSurfaceResultV1::Worktree(_)
        | NativeIntegrationSurfaceResultV1::Unavailable { .. }
        | NativeIntegrationSurfaceResultV1::Preview(_)
        | NativeIntegrationSurfaceResultV1::Receipt(_) => Ok(Vec::new()),
    }
}

fn receipt_envelopes(
    identity: &ObservabilityProducerIdentityV1,
    scope_ref: &str,
    receipt: &NativeIntegrationReceiptProjectionV1,
    durable_preview: Option<&NativeIntegrationPreviewV1>,
) -> Result<Vec<ObservabilityEnvelopeV1>, &'static str> {
    let anchor = format!(
        "native-transaction:{}",
        receipt.status.transaction_id.as_str()
    );
    let (result, terminal_result) = receipt_terminal(receipt.terminal_outcome);
    let (operation, dependency_commits, coverage) = match durable_preview {
        Some(preview) => {
            let (_, operation, _) = preview_disposition(&preview.disposition);
            let dependency_commits = u16::try_from(preview.ordered_commits.len())
                .map_err(|_| "native_integration_dependency_count")?;
            (
                operation,
                dependency_commits,
                preview_coverage(&preview.disposition),
            )
        }
        None => (
            IntegrationOperationKindV1::Unknown,
            0,
            CoverageStateV1::Partial,
        ),
    };
    let apply = WorkIntegrationTransitionObservedV1 {
        phase: IntegrationPhaseV1::ApplyTerminal,
        result,
        operation,
        source_scope: IntegrationScopeClassV1::Repository,
        target_scope: IntegrationScopeClassV1::Repository,
        dependency_commits_eligible: dependency_commits,
        dependency_commits_observed: dependency_commits,
        required_checks_eligible: 0,
        required_checks_observed: 0,
        owner_receipt: IntegrationOwnerReceiptV1::GitApply,
        coverage,
        local_anchor_refs: vec![anchor.clone()],
    };
    let mut envelopes = vec![transition_envelope(
        identity,
        scope_ref,
        &format!("{anchor}:apply-terminal"),
        "apply_native_integration",
        receipt.completed_at,
        terminal_result,
        apply,
    )?];
    if receipt.terminal_outcome == NativeIntegrationTerminalOutcomeV1::Committed {
        let integrated = WorkIntegrationTransitionObservedV1 {
            phase: IntegrationPhaseV1::NativeIntegratedObserved,
            result: IntegrationResultV1::Succeeded,
            operation,
            source_scope: IntegrationScopeClassV1::Repository,
            target_scope: IntegrationScopeClassV1::Repository,
            dependency_commits_eligible: dependency_commits,
            dependency_commits_observed: dependency_commits,
            required_checks_eligible: 0,
            required_checks_observed: 0,
            owner_receipt: IntegrationOwnerReceiptV1::NativeGitObservation,
            coverage,
            local_anchor_refs: vec![anchor.clone()],
        };
        envelopes.push(transition_envelope(
            identity,
            scope_ref,
            &format!("{anchor}:native-integrated"),
            "apply_native_integration",
            receipt.completed_at,
            Some(ObservabilityTerminalResultV1::Succeeded),
            integrated,
        )?);
    }
    Ok(envelopes)
}

fn preview_disposition(
    disposition: &NativeIntegrationPreviewDispositionV1,
) -> (
    IntegrationResultV1,
    IntegrationOperationKindV1,
    Option<ObservabilityTerminalResultV1>,
) {
    match disposition {
        NativeIntegrationPreviewDispositionV1::MechanicalIntegrationEligible(mode) => (
            IntegrationResultV1::Succeeded,
            match mode {
                MechanicalIntegrationModeV1::FastForward => IntegrationOperationKindV1::FastForward,
                MechanicalIntegrationModeV1::TwoParentMerge => {
                    IntegrationOperationKindV1::MergeCommit
                }
                MechanicalIntegrationModeV1::CherryPickExactCommits => {
                    IntegrationOperationKindV1::CherryPick
                }
            },
            Some(ObservabilityTerminalResultV1::Succeeded),
        ),
        NativeIntegrationPreviewDispositionV1::AlreadyIntegrated => (
            IntegrationResultV1::Succeeded,
            IntegrationOperationKindV1::Unknown,
            Some(ObservabilityTerminalResultV1::Succeeded),
        ),
        NativeIntegrationPreviewDispositionV1::NativeConflict { .. } => (
            IntegrationResultV1::Conflicted,
            IntegrationOperationKindV1::Unknown,
            Some(ObservabilityTerminalResultV1::Failed),
        ),
        NativeIntegrationPreviewDispositionV1::SemanticReviewRequired { .. }
        | NativeIntegrationPreviewDispositionV1::Partial { .. } => (
            IntegrationResultV1::Partial,
            IntegrationOperationKindV1::Unknown,
            Some(ObservabilityTerminalResultV1::Partial),
        ),
        NativeIntegrationPreviewDispositionV1::Unavailable { reason } => {
            unavailable_preview_result(*reason)
        }
    }
}

const fn preview_coverage(disposition: &NativeIntegrationPreviewDispositionV1) -> CoverageStateV1 {
    match disposition {
        NativeIntegrationPreviewDispositionV1::MechanicalIntegrationEligible(_)
        | NativeIntegrationPreviewDispositionV1::AlreadyIntegrated
        | NativeIntegrationPreviewDispositionV1::NativeConflict { .. }
        | NativeIntegrationPreviewDispositionV1::SemanticReviewRequired { .. } => {
            CoverageStateV1::Known
        }
        NativeIntegrationPreviewDispositionV1::Partial { .. } => CoverageStateV1::Partial,
        NativeIntegrationPreviewDispositionV1::Unavailable {
            reason: NativeIntegrationUnavailabilityV1::PartialEvidence,
        } => CoverageStateV1::Partial,
        NativeIntegrationPreviewDispositionV1::Unavailable {
            reason: NativeIntegrationUnavailabilityV1::StaleScope,
        } => CoverageStateV1::Stale,
        NativeIntegrationPreviewDispositionV1::Unavailable { .. } => CoverageStateV1::Unknown,
    }
}

const fn unavailable_preview_result(
    reason: NativeIntegrationUnavailabilityV1,
) -> (
    IntegrationResultV1,
    IntegrationOperationKindV1,
    Option<ObservabilityTerminalResultV1>,
) {
    let (result, terminal) = match reason {
        NativeIntegrationUnavailabilityV1::PartialEvidence => (
            IntegrationResultV1::Partial,
            ObservabilityTerminalResultV1::Partial,
        ),
        NativeIntegrationUnavailabilityV1::StaleScope => (
            IntegrationResultV1::Stale,
            ObservabilityTerminalResultV1::Unknown,
        ),
        NativeIntegrationUnavailabilityV1::Denied
        | NativeIntegrationUnavailabilityV1::SigningRequired => (
            IntegrationResultV1::Denied,
            ObservabilityTerminalResultV1::Denied,
        ),
        NativeIntegrationUnavailabilityV1::UnsupportedHooks => (
            IntegrationResultV1::Unsupported,
            ObservabilityTerminalResultV1::Unknown,
        ),
        NativeIntegrationUnavailabilityV1::DestinationOccupied => (
            IntegrationResultV1::Locked,
            ObservabilityTerminalResultV1::Unknown,
        ),
        NativeIntegrationUnavailabilityV1::NativeStateUnavailable
        | NativeIntegrationUnavailabilityV1::ResetRequired
        | NativeIntegrationUnavailabilityV1::DurabilityUncertain => (
            IntegrationResultV1::EffectUnknown,
            ObservabilityTerminalResultV1::Unknown,
        ),
    };
    (result, IntegrationOperationKindV1::Unknown, Some(terminal))
}

const fn receipt_terminal(
    outcome: NativeIntegrationTerminalOutcomeV1,
) -> (IntegrationResultV1, Option<ObservabilityTerminalResultV1>) {
    match outcome {
        NativeIntegrationTerminalOutcomeV1::Committed => (
            IntegrationResultV1::Succeeded,
            Some(ObservabilityTerminalResultV1::Succeeded),
        ),
        NativeIntegrationTerminalOutcomeV1::AbortedNoChange => (
            IntegrationResultV1::EffectUnknown,
            Some(ObservabilityTerminalResultV1::Unknown),
        ),
        NativeIntegrationTerminalOutcomeV1::RolledBack => (
            IntegrationResultV1::Failed,
            Some(ObservabilityTerminalResultV1::Failed),
        ),
        NativeIntegrationTerminalOutcomeV1::NeedsInspection => (
            IntegrationResultV1::EffectUnknown,
            Some(ObservabilityTerminalResultV1::Partial),
        ),
    }
}

fn transition_envelope(
    identity: &ObservabilityProducerIdentityV1,
    scope_ref: &str,
    owner_transition_ref: &str,
    operation: &str,
    occurred_at: UtcMicros,
    terminal_result: Option<ObservabilityTerminalResultV1>,
    observation: WorkIntegrationTransitionObservedV1,
) -> Result<ObservabilityEnvelopeV1, &'static str> {
    let coverage = observation.coverage;
    let payload = ObservabilityPayloadV1::WorkIntegrationTransition(observation);
    payload_envelope(
        identity,
        scope_ref,
        owner_transition_ref,
        operation,
        occurred_at,
        terminal_result,
        coverage,
        payload,
    )
}

/// Builds one payload-safe owner fact for the shared execution observability
/// producer. The owner transition reference is hashed into opaque event and
/// trace identities; it is never copied into an exportable envelope field.
/// The trace identity depends only on that stable owner reference. The event
/// identity additionally binds the exact times, terminal state, coverage, and
/// payload, so a revision of one logical owner receipt is a distinct event on
/// the same trace while an exact replay remains idempotent.
///
/// `event_time` and the valid-time bounds must come from the durable owner
/// transition. Callers must not substitute the current clock on replay.
pub fn execution_owner_fact_envelope(
    identity: &ObservabilityProducerIdentityV1,
    scope_ref: &str,
    owner_transition_ref: &str,
    operation: &str,
    event_time: UtcMicros,
    valid_from: Option<UtcMicros>,
    valid_until: Option<UtcMicros>,
    terminal_result: Option<ObservabilityTerminalResultV1>,
    coverage: CoverageStateV1,
    payload: ObservabilityPayloadV1,
) -> Result<ObservabilityEnvelopeV1, &'static str> {
    if identity.authorized_scope_ref != scope_ref
        || !tracedecay_domain::canonical_text::is_canonical_text_within(owner_transition_ref, 1_024)
    {
        return Err("execution_observation_owner_binding");
    }
    let event_digest = canonical_sha256(&(
        "tracedecay.execution-topology.owner-fact.v1",
        scope_ref,
        owner_transition_ref,
        operation,
        event_time,
        valid_from,
        valid_until,
        terminal_result,
        coverage,
        &payload,
    ))
    .map_err(|_| "execution_observation_identity")?;
    let trace_digest = canonical_sha256(&(
        "tracedecay.execution-topology.owner-trace.v1",
        scope_ref,
        owner_transition_ref,
    ))
    .map_err(|_| "execution_observation_trace_identity")?;
    let event_id = format!("execution-observation:{}", event_digest.as_str());
    let envelope = ObservabilityEnvelopeV1 {
        event_id: event_id.clone(),
        event_kind: payload.event_kind().to_owned(),
        schema_revision: 1,
        idempotency_key: event_id.clone(),
        trace_id: format!("execution-trace:{}", trace_digest.as_str()),
        scope_ref: scope_ref.to_owned(),
        capability: "execution-topology".to_owned(),
        operation: operation.to_owned(),
        event_time_micros: event_time.0,
        observation_time_micros: event_time.0,
        valid_from_micros: valid_from.map(|time| time.0),
        valid_until_micros: valid_until.map(|time| time.0),
        quantity: Some(1.0),
        unit: Some("events".to_owned()),
        terminal_result,
        producer_revision: identity.producer_revision.clone(),
        configuration_revision: identity.configuration_revision.clone(),
        policy_revision: identity.policy_revision.clone(),
        watermark: format!("{}:pending", identity.process_boot_id),
        coverage,
        sampling_probability: None,
        retention_class: ObservabilityRetentionClassV1::OptionalLocalDetail30d,
        emitted_count: 1,
        delayed_count: 0,
        dropped_count: 0,
        process_boot_id: identity.process_boot_id.clone(),
        // The producer replaces this placeholder while holding its emission
        // lock, before the envelope enters the bounded queue.
        producer_sequence: 1,
        payload,
    };
    envelope.validate()?;
    Ok(envelope)
}

fn payload_envelope(
    identity: &ObservabilityProducerIdentityV1,
    scope_ref: &str,
    owner_transition_ref: &str,
    operation: &str,
    occurred_at: UtcMicros,
    terminal_result: Option<ObservabilityTerminalResultV1>,
    coverage: CoverageStateV1,
    payload: ObservabilityPayloadV1,
) -> Result<ObservabilityEnvelopeV1, &'static str> {
    execution_owner_fact_envelope(
        identity,
        scope_ref,
        owner_transition_ref,
        operation,
        occurred_at,
        Some(occurred_at),
        Some(occurred_at),
        terminal_result,
        coverage,
        payload,
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tracedecay_application::{
        NativeIntegrationPreviewProjectionV1, NativeIntegrationStatusProjectionV1,
    };
    use tracedecay_domain::{
        ManifestDigest, NativeIntegrationPhaseV1, NativeIntegrationPreviewId,
        NativeIntegrationTransactionId, ProjectId, RefId, RepositoryId,
    };

    fn digest(byte: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
    }

    fn identity(scope_ref: &str) -> ObservabilityProducerIdentityV1 {
        ObservabilityProducerIdentityV1 {
            authorized_scope_ref: scope_ref.to_owned(),
            process_boot_id: "boot:native-integration-test".to_owned(),
            producer_revision: "native-integration-test.v1".to_owned(),
            configuration_revision: "native-integration-test-config.v1".to_owned(),
            policy_revision: "native-integration-test-policy.v1".to_owned(),
        }
    }

    fn preview() -> NativeIntegrationSurfaceResultV1 {
        NativeIntegrationSurfaceResultV1::Preview(NativeIntegrationPreviewProjectionV1 {
            preview_id: NativeIntegrationPreviewId::new("preview.native.fixture").unwrap(),
            preview_digest: digest('a'),
            selection: tracedecay_application::NativeIntegrationSnapshotProjectionV1 {
                selection_digest: digest('b'),
                project_id: ProjectId::new("private-project").unwrap(),
                repository_id: RepositoryId::new("private-repository").unwrap(),
                source_ref: RefId::new("private-source-ref").unwrap(),
                destination_ref: RefId::new("private-target-ref").unwrap(),
                inventory_epoch: tracedecay_domain::WorktreeInventoryEpoch::new(1).unwrap(),
                frozen_at: UtcMicros(10),
            },
            disposition: NativeIntegrationPreviewDispositionV1::MechanicalIntegrationEligible(
                MechanicalIntegrationModeV1::FastForward,
            ),
            ordered_commit_count: 2,
            created_at: UtcMicros(20),
            expires_at: UtcMicros(30),
        })
    }

    fn committed_receipt() -> NativeIntegrationSurfaceResultV1 {
        NativeIntegrationSurfaceResultV1::Receipt(NativeIntegrationReceiptProjectionV1 {
            status: NativeIntegrationStatusProjectionV1 {
                transaction_id: NativeIntegrationTransactionId::new("transaction.native.fixture")
                    .unwrap(),
                preview_id: NativeIntegrationPreviewId::new("preview.native.fixture").unwrap(),
                preview_digest: digest('a'),
                repository_id: RepositoryId::new("private-repository").unwrap(),
                destination_ref: RefId::new("private-target-ref").unwrap(),
                phase: NativeIntegrationPhaseV1::Terminal,
                phase_revision: 5,
                cancellation_requested: false,
                terminal_outcome: Some(NativeIntegrationTerminalOutcomeV1::Committed),
                updated_at: UtcMicros(40),
            },
            terminal_outcome: NativeIntegrationTerminalOutcomeV1::Committed,
            final_ref_tip: "private-final-object".to_owned(),
            final_tree: "private-final-tree".to_owned(),
            completed_at: UtcMicros(40),
            receipt_digest: digest('c'),
        })
    }

    #[test]
    fn durable_preview_emits_a_bounded_private_safe_dry_run_transition() {
        let envelopes = native_integration_envelopes(
            &identity("project.scope"),
            "project.scope",
            "preflight_native_integration",
            &preview(),
            None,
        )
        .expect("preview observation");
        assert_eq!(envelopes.len(), 1);
        let envelope = &envelopes[0];
        let ObservabilityPayloadV1::WorkIntegrationTransition(payload) = &envelope.payload else {
            panic!("wrong payload family");
        };
        assert_eq!(payload.phase, IntegrationPhaseV1::DryRunTerminal);
        assert_eq!(payload.operation, IntegrationOperationKindV1::FastForward);
        assert_eq!(payload.dependency_commits_eligible, 2);
        assert_eq!(envelope.coverage, CoverageStateV1::Known);
        let wire = serde_json::to_string(envelope).expect("serialize envelope");
        for prohibited in [
            "private-project",
            "private-repository",
            "private-source-ref",
            "private-target-ref",
        ] {
            assert!(!wire.contains(prohibited), "leaked {prohibited}");
        }
    }

    #[test]
    fn committed_owner_receipt_emits_apply_and_native_observation_without_git_objects() {
        let envelopes = native_integration_envelopes(
            &identity("project.scope"),
            "project.scope",
            "apply_native_integration",
            &committed_receipt(),
            None,
        )
        .expect("receipt observations");
        assert_eq!(envelopes.len(), 2);
        let phases = envelopes
            .iter()
            .map(|envelope| match &envelope.payload {
                ObservabilityPayloadV1::WorkIntegrationTransition(payload) => payload.phase,
                _ => panic!("wrong payload family"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            phases,
            vec![
                IntegrationPhaseV1::ApplyTerminal,
                IntegrationPhaseV1::NativeIntegratedObserved,
            ]
        );
        let wire = serde_json::to_string(&envelopes).expect("serialize envelopes");
        assert!(!wire.contains("private-final-object"));
        assert!(!wire.contains("private-final-tree"));
        assert!(!wire.contains("private-target-ref"));
    }

    #[test]
    fn read_or_unsupported_owner_result_does_not_fabricate_a_transition() {
        let NativeIntegrationSurfaceResultV1::Receipt(receipt) = committed_receipt() else {
            unreachable!()
        };
        let status = NativeIntegrationSurfaceResultV1::Status(receipt.status);
        assert!(
            native_integration_envelopes(
                &identity("project.scope"),
                "project.scope",
                "native_integration_status",
                &status,
                None,
            )
            .expect("typed unavailable source")
            .is_empty()
        );
        assert!(
            native_integration_envelopes(
                &identity("project.scope"),
                "project.scope",
                "wrong_operation",
                &preview(),
                None,
            )
            .expect("wrong owner operation")
            .is_empty()
        );
    }

    #[test]
    fn replayed_owner_receipt_builds_byte_identical_idempotent_envelopes() {
        let first = native_integration_envelopes(
            &identity("project.scope"),
            "project.scope",
            "apply_native_integration",
            &committed_receipt(),
            None,
        )
        .expect("first emission");
        let replay = native_integration_envelopes(
            &identity("project.scope"),
            "project.scope",
            "apply_native_integration",
            &committed_receipt(),
            None,
        )
        .expect("replay emission");
        assert_eq!(first, replay, "owner receipt time is canonical");
    }

    #[test]
    fn corrected_owner_fact_keeps_trace_and_advances_event_identity() {
        let open = ObservabilityPayloadV1::WorkBlockedInterval(
            tracedecay_domain::WorkBlockedIntervalObservedV1 {
                cause: tracedecay_domain::BlockedCauseV1::NeedsInput,
                interval_revision: 1,
                valid_from_micros: 10,
                valid_until_micros: None,
                coverage: CoverageStateV1::Known,
            },
        );
        let closed = ObservabilityPayloadV1::WorkBlockedInterval(
            tracedecay_domain::WorkBlockedIntervalObservedV1 {
                cause: tracedecay_domain::BlockedCauseV1::NeedsInput,
                interval_revision: 2,
                valid_from_micros: 10,
                valid_until_micros: Some(20),
                coverage: CoverageStateV1::Known,
            },
        );
        let open = execution_owner_fact_envelope(
            &identity("project.scope"),
            "project.scope",
            "blocked-interval:fixture",
            "pause_work_run",
            UtcMicros(10),
            Some(UtcMicros(10)),
            None,
            None,
            CoverageStateV1::Known,
            open,
        )
        .expect("open interval");
        let replay = execution_owner_fact_envelope(
            &identity("project.scope"),
            "project.scope",
            "blocked-interval:fixture",
            "pause_work_run",
            UtcMicros(10),
            Some(UtcMicros(10)),
            None,
            None,
            CoverageStateV1::Known,
            open.payload.clone(),
        )
        .expect("open replay");
        let closed = execution_owner_fact_envelope(
            &identity("project.scope"),
            "project.scope",
            "blocked-interval:fixture",
            "pause_work_run",
            UtcMicros(20),
            Some(UtcMicros(10)),
            Some(UtcMicros(20)),
            None,
            CoverageStateV1::Known,
            closed,
        )
        .expect("closed interval");

        assert_eq!(open, replay, "exact owner replay must be byte-identical");
        assert_eq!(open.trace_id, closed.trace_id);
        assert_ne!(open.event_id, closed.event_id);
        assert_ne!(open.idempotency_key, closed.idempotency_key);
    }
}
