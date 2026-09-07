//! Behavioral contract for the execution-topology metrics projection.

use std::collections::BTreeSet;
#[path = "execution_topology_metrics/stack_drift.rs"]
mod stack_drift;
#[path = "execution_topology_metrics/support.rs"]
mod support;
use support::{CountingObservations, NeverRollupPort};

use tracedecay_application::{
    ApplicationContractError, ApplicationProblem, CancellationContext, CapabilityGrantSnapshot,
    Deadline, DisclosureClass, EXECUTION_TOPOLOGY_EVENT_KINDS_V1, ExecutionBlockedCauseV1,
    ExecutionConcurrencyPhaseV1, ExecutionConflictKindV1, ExecutionConflictOutcomeV1,
    ExecutionFanoutPhaseV1, ExecutionGitHubStackCapabilityV1, ExecutionMetricUnavailableV1,
    ExecutionTopologyDimensionV1, ExecutionTopologyMeasurementV1,
    ExecutionTopologyMetricsRequestV1, ExecutionTopologyMetricsV1, ExecutionWidthBucketV1,
    MAX_EXECUTION_TOPOLOGY_EVENTS_V1, ObservabilityFuture, ObservabilityHorizonV1,
    ObservabilityPageV1, ObservabilityQueryPort, ObservabilityQueryV1, RequestContext, RequestId,
    ResolvedScope, execution_topology_rollup_metrics,
};
use tracedecay_domain::{
    ActorId, BlockedCauseV1, ConflictAdjudicatorV1, ConflictKindV1, ConflictOutcomeV1,
    ConflictPredictionV1, ConflictScoreKindV1, CoverageStateV1, DeliveryEventClassV1,
    DeliverySurfaceFamilyV1, ExecutionPlacementV1, ExecutionTopologyKindV1,
    ExecutionTopologySampledV1, GitHubStackCapabilityObservedV1, GitHubStackCapabilityV1,
    IntegrationStrategyV1, ManifestDigest, ObservabilityEnvelopeV1, ObservabilityPayloadV1,
    ObservabilityRetentionClassV1, ProjectId, RepositoryId, ReviewTopologyV1,
    TelemetryDropObservedV1, UtcMicros, WorkBlockedIntervalObservedV1, WorkConflictOutcomeLinkedV1,
    WorkConflictPredictionObservedV1, WorkDeliveryFanoutObservedV1, WorkTopologyBranchV1,
    WorktreeId,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

fn context() -> RequestContext {
    context_with(
        true,
        UtcMicros(i64::MAX),
        Deadline::new(UtcMicros(i64::MAX)).unwrap(),
        CancellationContext::active("cancel.topology.metrics").unwrap(),
    )
}

fn context_with(
    allows_topology: bool,
    grant_expires_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> RequestContext {
    let scope = ResolvedScope::new(
        id::<ProjectId>("project.topology.metrics"),
        id::<RepositoryId>("repository.topology.metrics"),
        id::<WorktreeId>("worktree.topology.metrics"),
        None,
    )
    .unwrap();
    let capability = CapabilityId::new(if allows_topology {
        "capability.work.topology_metrics"
    } else {
        "capability.work.snapshot"
    })
    .unwrap();
    let use_case = UseCaseId::new(if allows_topology {
        "use-case.work.topology_metrics"
    } else {
        "use-case.work.snapshot"
    })
    .unwrap();
    let grant = CapabilityGrantSnapshot::new(
        id("grant.topology.metrics"),
        1,
        ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
        id::<ActorId>("actor.issuer"),
        UtcMicros(1),
        grant_expires_at,
        scope.clone(),
        BTreeSet::from([capability]),
        BTreeSet::from([use_case]),
        DisclosureClass::Sensitive,
    )
    .unwrap();
    RequestContext::new(
        id::<ActorId>("actor.metrics.reader"),
        scope,
        grant,
        RequestId::new("request.topology.metrics").unwrap(),
        deadline,
        cancellation,
    )
    .unwrap()
}

/// A recorded envelope. Every identifier is canonical so the projection reads
/// the same bytes a real observation authority would have persisted.
fn envelope(
    sequence: u64,
    trace: &str,
    payload: ObservabilityPayloadV1,
    valid: Option<(i64, i64)>,
) -> ObservabilityEnvelopeV1 {
    let envelope = ObservabilityEnvelopeV1 {
        event_id: format!("event.{sequence}"),
        event_kind: payload.event_kind().to_owned(),
        schema_revision: 1,
        idempotency_key: format!("idempotency.{sequence}"),
        trace_id: trace.to_owned(),
        scope_ref: context().scope().project_id.as_str().to_owned(),
        capability: "capability.work".to_owned(),
        operation: "operation.work.sample".to_owned(),
        event_time_micros: 1_000,
        observation_time_micros: 2_000,
        valid_from_micros: valid.map(|(from, _)| from),
        valid_until_micros: valid.map(|(_, until)| until),
        quantity: None,
        unit: None,
        terminal_result: None,
        producer_revision: "producer.v1".to_owned(),
        configuration_revision: "configuration.v1".to_owned(),
        policy_revision: "policy.v1".to_owned(),
        watermark: format!("watermark.{sequence}"),
        coverage: CoverageStateV1::Known,
        sampling_probability: None,
        retention_class: ObservabilityRetentionClassV1::LocalRollup395d,
        emitted_count: 1,
        delayed_count: 0,
        dropped_count: 0,
        process_boot_id: "boot.fixture".to_owned(),
        producer_sequence: sequence,
        payload,
    };
    envelope
        .validate()
        .expect("fixture envelope satisfies the domain contract");
    envelope
}

fn topology_sample(
    requested: u16,
    admitted: u16,
    active: u16,
    useful: u16,
    anchors: Vec<String>,
) -> ObservabilityPayloadV1 {
    ObservabilityPayloadV1::ExecutionTopology(ExecutionTopologySampledV1 {
        topology: ExecutionTopologyKindV1::Parallel,
        placement: ExecutionPlacementV1::LinkedWorktree,
        branch_topology: WorkTopologyBranchV1::IndependentBranches,
        review_topology: ReviewTopologyV1::IndependentReview,
        integration_strategy: IntegrationStrategyV1::FastForwardOnly,
        requested_width: requested,
        accepted_width: requested,
        admitted_width: admitted,
        active_width: active,
        useful_width: useful,
        runnable_count: active,
        blocked_count: 0,
        shared_authority_serialized_count: 0,
        local_anchor_refs: anchors,
    })
}

fn blocked(cause: BlockedCauseV1, from: i64, until: i64) -> ObservabilityPayloadV1 {
    ObservabilityPayloadV1::WorkBlockedInterval(WorkBlockedIntervalObservedV1 {
        cause,
        interval_revision: 1,
        valid_from_micros: from,
        valid_until_micros: Some(until),
        coverage: CoverageStateV1::Known,
    })
}

fn page(events: Vec<ObservabilityEnvelopeV1>) -> ObservabilityPageV1 {
    let event_cursors = events
        .iter()
        .map(|event| format!("cursor.{}", event.producer_sequence))
        .collect();
    ObservabilityPageV1 {
        events,
        event_cursors,
        watermark: "watermark.page".to_owned(),
        coverage: CoverageStateV1::Known,
        next_watermark: None,
    }
}

enum Observations {
    Page(ObservabilityPageV1),
    Refused,
}

impl ObservabilityQueryPort for Observations {
    fn query<'a>(
        &'a self,
        query: ObservabilityQueryV1,
    ) -> ObservabilityFuture<'a, ObservabilityPageV1> {
        assert_eq!(
            query.authorized_scope_ref,
            context().scope().project_id.as_str()
        );
        let mut expected_kinds = EXECUTION_TOPOLOGY_EVENT_KINDS_V1
            .iter()
            .map(|kind| (*kind).to_owned())
            .collect::<Vec<_>>();
        expected_kinds.push("telemetry.drop.observed.v1".to_owned());
        assert_eq!(query.event_kinds, expected_kinds);
        let outcome = match self {
            Self::Page(page) => Ok(page.clone()),
            Self::Refused => Err(ApplicationContractError::Domain(
                "observation store is unavailable".to_owned(),
            )),
        };
        Box::pin(async move { outcome })
    }
}

fn request() -> ExecutionTopologyMetricsRequestV1 {
    ExecutionTopologyMetricsRequestV1 {
        horizon: ObservabilityHorizonV1 {
            since_micros: 0,
            until_micros: 100_000,
        },
        max_events: 1_000,
    }
}

async fn read(observations: &Observations) -> ExecutionTopologyMetricsV1 {
    execution_topology_rollup_metrics(&NeverRollupPort, observations, &context(), &request())
        .await
        .expect("an authorized read over a valid horizon is admitted")
}

fn find<'a>(
    model: &'a ExecutionTopologyMetricsV1,
    metric: &str,
    dimensions: &[ExecutionTopologyDimensionV1],
) -> &'a ExecutionTopologyMeasurementV1 {
    model
        .measurements
        .iter()
        .find(|measurement| {
            measurement.value.metric == metric && measurement.dimensions == dimensions
        })
        .unwrap_or_else(|| panic!("descriptor {metric} is present with the requested dimensions"))
}

#[tokio::test]
async fn an_empty_horizon_is_a_typed_absence_for_every_descriptor_not_a_zero() {
    let model = read(&Observations::Page(page(Vec::new()))).await;

    assert!(!model.measurements.is_empty());
    assert!(
        model
            .measurements
            .iter()
            .all(|measurement| { measurement.value.metric != "work_ready_to_integrated_seconds" })
    );
    for measurement in &model.measurements {
        assert_eq!(
            measurement.value.value, None,
            "{} rendered a value without evidence",
            measurement.value.metric
        );
        assert_eq!(
            measurement.unavailable,
            Some(ExecutionMetricUnavailableV1::NoEligibleEvidence),
            "{} lost its typed absence reason",
            measurement.value.metric
        );
        assert_eq!(
            measurement.value.unavailable_reason.as_deref(),
            Some("no_eligible_evidence")
        );
    }
}

#[tokio::test]
async fn admission_refuses_missing_capability_cancellation_deadline_and_grant_expiry_before_read() {
    let observations = CountingObservations::new();
    let cases = [
        (
            context_with(
                false,
                UtcMicros(i64::MAX),
                Deadline::new(UtcMicros(i64::MAX)).unwrap(),
                CancellationContext::active("cancel.denied").unwrap(),
            ),
            "not_found_or_not_authorized",
        ),
        (
            context_with(
                true,
                UtcMicros(i64::MAX),
                Deadline::new(UtcMicros(i64::MAX)).unwrap(),
                CancellationContext::cancelled("cancel.cancelled", UtcMicros(2)).unwrap(),
            ),
            "cancelled",
        ),
        (
            context_with(
                true,
                UtcMicros(i64::MAX),
                Deadline::new(UtcMicros(2)).unwrap(),
                CancellationContext::active("cancel.deadline").unwrap(),
            ),
            "timed_out",
        ),
        (
            context_with(
                true,
                UtcMicros(2),
                Deadline::new(UtcMicros(i64::MAX)).unwrap(),
                CancellationContext::active("cancel.grant-expiry").unwrap(),
            ),
            "timed_out",
        ),
    ];

    for (context, expected_code) in cases {
        let problem = execution_topology_rollup_metrics(
            &NeverRollupPort,
            &observations,
            &context,
            &request(),
        )
        .await
        .expect_err("inadmissible requests are refused");
        assert_eq!(problem.canonical_code(), expected_code);
    }
    assert_eq!(observations.query_count(), 0);
}

#[tokio::test]
async fn emitted_delayed_dropped_and_sampled_evidence_weaken_family_coverage() {
    let mut dropped = envelope(
        1,
        "trace.dropped",
        topology_sample(2, 2, 1, 1, Vec::new()),
        Some((0, 1_000_000)),
    );
    dropped.dropped_count = 2;
    let dropped_model = read(&Observations::Page(page(vec![dropped]))).await;
    assert_eq!(dropped_model.coverage.eligible, Some(3));
    assert_eq!(dropped_model.coverage.observed, 1);
    assert_eq!(dropped_model.coverage.completed, 1);
    assert_eq!(dropped_model.coverage.unknown, 2);
    assert_eq!(dropped_model.coverage.state, CoverageStateV1::Partial);
    assert_eq!(dropped_model.emission_coverage.emitted, Some(1));
    assert_eq!(dropped_model.emission_coverage.delayed, Some(0));
    assert_eq!(dropped_model.emission_coverage.dropped, Some(2));
    assert!(!dropped_model.current);
    assert!(
        dropped_model
            .measurements
            .iter()
            .all(|measurement| measurement.value.value.is_none())
    );

    let mut delayed = envelope(
        2,
        "trace.delayed",
        topology_sample(2, 2, 1, 1, Vec::new()),
        Some((0, 1_000_000)),
    );
    delayed.delayed_count = 1;
    let delayed_model = read(&Observations::Page(page(vec![delayed]))).await;
    assert_eq!(delayed_model.coverage.eligible, Some(1));
    assert_eq!(delayed_model.coverage.observed, 1);
    assert_eq!(delayed_model.coverage.completed, 0);
    assert_eq!(delayed_model.coverage.state, CoverageStateV1::Partial);
    assert_eq!(delayed_model.emission_coverage.delayed, Some(1));
    assert!(!delayed_model.current);

    let mut sampled = envelope(
        3,
        "trace.sampled",
        topology_sample(2, 2, 1, 1, Vec::new()),
        Some((0, 1_000_000)),
    );
    sampled.coverage = CoverageStateV1::Sampled;
    sampled.sampling_probability = Some(0.5);
    let sampled_model = read(&Observations::Page(page(vec![sampled]))).await;
    assert_eq!(sampled_model.coverage.eligible, None);
    assert_eq!(sampled_model.coverage.observed, 1);
    assert_eq!(sampled_model.coverage.state, CoverageStateV1::Sampled);
    assert_eq!(sampled_model.emission_coverage.sampled_events, Some(1));
    assert!(!sampled_model.current);
}

#[tokio::test]
async fn explicit_drop_receipt_and_next_envelope_carrier_are_counted_once() {
    let mut drop_receipt = envelope(
        2,
        "trace.drop-receipt",
        ObservabilityPayloadV1::TelemetryDrop(TelemetryDropObservedV1 {
            first_missing_sequence: 1,
            last_missing_sequence: 2,
            proved_drop_lower_bound: 2,
            clean_shutdown_observed: false,
        }),
        None,
    );
    drop_receipt.dropped_count = 2;
    drop_receipt.coverage = CoverageStateV1::Partial;

    let mut carrier = envelope(
        3,
        "trace.carrier",
        topology_sample(2, 2, 1, 1, Vec::new()),
        Some((0, 1_000_000)),
    );
    carrier.dropped_count = 2;
    carrier.coverage = CoverageStateV1::Partial;

    let model = read(&Observations::Page(page(vec![drop_receipt, carrier]))).await;

    assert_eq!(model.coverage.observed, 1);
    assert_eq!(model.coverage.unknown, 2);
    assert_eq!(model.emission_coverage.dropped, Some(2));
    assert_eq!(model.drill_anchors.len(), 1);
    assert_eq!(model.drill_anchors[0].cursor, "cursor.3");
}

#[tokio::test]
async fn replayed_idempotency_identity_is_excluded_without_double_counting() {
    let original = envelope(
        1,
        "trace.replay",
        topology_sample(2, 2, 1, 1, Vec::new()),
        Some((0, 1_000_000)),
    );
    let replay = original.clone();
    let mut replayed_page = page(vec![original, replay]);
    replayed_page.event_cursors[1] = "cursor.replay".to_owned();
    let model = read(&Observations::Page(replayed_page)).await;

    assert_eq!(model.coverage.observed, 1);
    assert_eq!(model.coverage.excluded, 1);
    let active = find(
        &model,
        "work_execution_concurrency_width",
        &[
            ExecutionTopologyDimensionV1::ConcurrencyPhase(ExecutionConcurrencyPhaseV1::Active),
            ExecutionTopologyDimensionV1::WidthBucket(ExecutionWidthBucketV1::One),
        ],
    );
    assert_eq!(active.value.value, None);
    assert_eq!(active.value.denominator_value, None);
    assert_eq!(active.value.coverage.state, CoverageStateV1::Unknown);
    assert_eq!(
        active.unavailable,
        Some(ExecutionMetricUnavailableV1::SupportFloorUnmet)
    );
}

#[tokio::test]
async fn github_stack_capability_is_counted_as_an_observed_topology_family() {
    let capability =
        ObservabilityPayloadV1::GitHubStackCapability(GitHubStackCapabilityObservedV1 {
            capability: GitHubStackCapabilityV1::PrivatePreviewDisabled,
            probe_revision: "github-stack-probe.v1".to_owned(),
            standard_git_fallback_available: true,
            other_forge_fallback_available: false,
            coverage: CoverageStateV1::Known,
        });
    let model = read(&Observations::Page(page(vec![envelope(
        1,
        "trace.github-stack",
        capability,
        None,
    )])))
    .await;

    assert_eq!(model.coverage.eligible, Some(1));
    assert_eq!(model.coverage.observed, 1);
    assert_eq!(model.coverage.completed, 1);
    assert!(model.current);
    assert_eq!(
        model.github_stack_capability.capability,
        Some(ExecutionGitHubStackCapabilityV1::PrivatePreviewDisabled)
    );
    assert_eq!(
        model
            .github_stack_capability
            .standard_git_fallback_available,
        Some(true)
    );
    assert_eq!(
        model.github_stack_capability.other_forge_fallback_available,
        Some(false)
    );
    assert_eq!(model.github_stack_capability.unavailable, None);
}

#[tokio::test]
async fn concurrency_width_is_duration_weighted_while_fanout_width_counts_samples() {
    let model = read(&Observations::Page(page(vec![
        envelope(
            1,
            "trace.a",
            topology_sample(4, 4, 2, 1, Vec::new()),
            Some((0, 1_000_000)),
        ),
        envelope(
            2,
            "trace.b",
            topology_sample(4, 4, 2, 1, Vec::new()),
            Some((0, 3_000_000)),
        ),
        envelope(
            3,
            "trace.padding-3",
            topology_sample(4, 4, 2, 1, Vec::new()),
            Some((0, 1)),
        ),
        envelope(
            4,
            "trace.padding-4",
            topology_sample(4, 4, 2, 1, Vec::new()),
            Some((0, 1)),
        ),
        envelope(
            5,
            "trace.padding-5",
            topology_sample(4, 4, 2, 1, Vec::new()),
            Some((0, 1)),
        ),
    ])))
    .await;

    let admitted = find(
        &model,
        "work_execution_concurrency_width",
        &[
            ExecutionTopologyDimensionV1::ConcurrencyPhase(ExecutionConcurrencyPhaseV1::Admitted),
            ExecutionTopologyDimensionV1::WidthBucket(ExecutionWidthBucketV1::From3To4),
        ],
    );
    // Four microseconds of recorded interval, not two samples.
    assert_eq!(admitted.value.value, Some(4_000_003.0));
    assert_eq!(admitted.value.unit, "microseconds");

    let peak = find(
        &model,
        "work_execution_fanout_width",
        &[
            ExecutionTopologyDimensionV1::FanoutPhase(ExecutionFanoutPhaseV1::PeakActive),
            ExecutionTopologyDimensionV1::WidthBucket(ExecutionWidthBucketV1::Two),
        ],
    );
    assert_eq!(peak.value.value, Some(5.0));
    assert_eq!(peak.value.unit, "events");

    let ratio = find(&model, "work_execution_useful_concurrency_ratio", &[]);
    // One useful attempt out of four admitted, over both weighted intervals.
    assert_eq!(ratio.value.value, Some(0.25));
    assert_eq!(ratio.unavailable, None);
}

#[tokio::test]
async fn a_sample_without_a_bounded_interval_is_censored_not_zero_duration() {
    let model = read(&Observations::Page(page(vec![
        envelope(
            1,
            "trace.a",
            topology_sample(4, 4, 2, 1, Vec::new()),
            Some((0, 1_000_000)),
        ),
        envelope(2, "trace.b", topology_sample(4, 4, 2, 1, Vec::new()), None),
        envelope(
            3,
            "trace.padding-3",
            topology_sample(4, 4, 2, 1, Vec::new()),
            Some((0, 1)),
        ),
        envelope(
            4,
            "trace.padding-4",
            topology_sample(4, 4, 2, 1, Vec::new()),
            Some((0, 1)),
        ),
        envelope(
            5,
            "trace.padding-5",
            topology_sample(4, 4, 2, 1, Vec::new()),
            Some((0, 1)),
        ),
    ])))
    .await;

    let admitted = find(
        &model,
        "work_execution_concurrency_width",
        &[ExecutionTopologyDimensionV1::ConcurrencyPhase(
            ExecutionConcurrencyPhaseV1::Admitted,
        )],
    );
    assert_eq!(admitted.value.value, None);
    assert_eq!(
        admitted.unavailable,
        Some(ExecutionMetricUnavailableV1::SupportFloorUnmet)
    );
    assert_eq!(admitted.value.coverage.censored, 0);
    assert_eq!(admitted.value.coverage.unknown, 1);

    let peak = find(
        &model,
        "work_execution_fanout_width",
        &[
            ExecutionTopologyDimensionV1::FanoutPhase(ExecutionFanoutPhaseV1::PeakActive),
            ExecutionTopologyDimensionV1::WidthBucket(ExecutionWidthBucketV1::Two),
        ],
    );
    assert_eq!(peak.value.value, Some(5.0));
}

#[tokio::test]
async fn blocked_wall_time_unions_while_per_cause_time_attributes_and_may_exceed_it() {
    let mut events = Vec::new();
    for index in 0..5_u64 {
        events.push(envelope(
            index + 1,
            &format!("trace.dependency.{index}"),
            blocked(
                BlockedCauseV1::Dependency,
                0,
                if index == 0 { 2_000_000 } else { 1_000_000 },
            ),
            None,
        ));
        events.push(envelope(
            index + 6,
            &format!("trace.review.{index}"),
            blocked(BlockedCauseV1::Review, 1_000_000, 3_000_000),
            None,
        ));
    }
    let model = read(&Observations::Page(page(events))).await;

    let wall = find(&model, "work_blocked_wall_seconds", &[]);
    assert_eq!(wall.value.value, Some(3.0));

    let dependency = find(
        &model,
        "work_blocked_cause_seconds",
        &[ExecutionTopologyDimensionV1::BlockedCause(
            ExecutionBlockedCauseV1::Dependency,
        )],
    );
    let review = find(
        &model,
        "work_blocked_cause_seconds",
        &[ExecutionTopologyDimensionV1::BlockedCause(
            ExecutionBlockedCauseV1::Review,
        )],
    );
    assert_eq!(dependency.value.value, Some(2.0));
    assert_eq!(review.value.value, Some(2.0));
    // Overlapping causes sum above wall time by construction.
    assert!(
        dependency.value.value.unwrap() + review.value.value.unwrap() > wall.value.value.unwrap()
    );
}

#[tokio::test]
async fn conflicting_same_revision_blocked_intervals_are_order_independent_and_unavailable() {
    let first = envelope(
        1,
        "trace.blocked-correction",
        blocked(BlockedCauseV1::Dependency, 0, 2_000_000),
        None,
    );
    let conflicting = envelope(
        2,
        "trace.blocked-correction",
        blocked(BlockedCauseV1::Dependency, 0, 3_000_000),
        None,
    );
    let forward = read(&Observations::Page(page(vec![
        first.clone(),
        conflicting.clone(),
    ])))
    .await;
    let reverse = read(&Observations::Page(page(vec![conflicting, first]))).await;

    assert_eq!(forward.measurements, reverse.measurements);
    let wall = find(&forward, "work_blocked_wall_seconds", &[]);
    assert_eq!(wall.value.value, None);
    assert_eq!(
        wall.unavailable,
        Some(ExecutionMetricUnavailableV1::SupportFloorUnmet)
    );
    assert_eq!(wall.value.coverage.unknown, 1);
}

#[tokio::test]
async fn conflict_cells_suppress_below_five_and_precision_remains_unavailable() {
    let prediction =
        ObservabilityPayloadV1::WorkConflictPrediction(WorkConflictPredictionObservedV1 {
            prediction_ref: "prediction.a".to_owned(),
            kind: ConflictKindV1::Mechanical,
            prediction: ConflictPredictionV1::Conflict,
            score_kind: ConflictScoreKindV1::Rule,
            descriptor_revision: "conflict-descriptor.v1".to_owned(),
            calibration_revision: "conflict-calibration.v1".to_owned(),
            eligible_relation_count: 1,
            expires_at_micros: 50_000,
            coverage: CoverageStateV1::Known,
            local_anchor_refs: Vec::new(),
        });
    let outcome = ObservabilityPayloadV1::WorkConflictOutcome(WorkConflictOutcomeLinkedV1 {
        prediction_ref: "prediction.a".to_owned(),
        kind: ConflictKindV1::Mechanical,
        outcome: ConflictOutcomeV1::Conflict,
        adjudicator: ConflictAdjudicatorV1::NativeGit,
        horizon_micros: 500,
        coverage: CoverageStateV1::Known,
        correction_revision: 1,
    });
    let model = read(&Observations::Page(page(vec![
        envelope(1, "trace.a", prediction, None),
        envelope(2, "trace.a", outcome, None),
    ])))
    .await;

    let total = find(
        &model,
        "work_conflict_prediction_total",
        &[
            ExecutionTopologyDimensionV1::ConflictKind(ExecutionConflictKindV1::Mechanical),
            ExecutionTopologyDimensionV1::ConflictOutcome(ExecutionConflictOutcomeV1::Conflict),
        ],
    );
    assert_eq!(total.value.value, None);
    assert_eq!(total.value.coverage.state, CoverageStateV1::Unknown);
    assert_eq!(
        total.unavailable,
        Some(ExecutionMetricUnavailableV1::SupportFloorUnmet)
    );

    let precision = find(
        &model,
        "work_conflict_prediction_precision",
        &[ExecutionTopologyDimensionV1::ConflictKind(
            ExecutionConflictKindV1::Mechanical,
        )],
    );
    // One adjudicated case is real evidence and a perfect score is not: the
    // support floor refuses rather than rendering 100%.
    assert_eq!(precision.value.value, None);
    assert_eq!(
        precision.unavailable,
        Some(ExecutionMetricUnavailableV1::SupportFloorUnmet)
    );
}

#[tokio::test]
async fn late_conflict_correction_rebuilds_to_the_same_highest_revision() {
    let prediction = envelope(
        1,
        "trace.correction",
        ObservabilityPayloadV1::WorkConflictPrediction(WorkConflictPredictionObservedV1 {
            prediction_ref: "prediction.correction".to_owned(),
            kind: ConflictKindV1::Mechanical,
            prediction: ConflictPredictionV1::Conflict,
            score_kind: ConflictScoreKindV1::Rule,
            descriptor_revision: "conflict-descriptor.v1".to_owned(),
            calibration_revision: "conflict-calibration.v1".to_owned(),
            eligible_relation_count: 1,
            expires_at_micros: 50_000,
            coverage: CoverageStateV1::Known,
            local_anchor_refs: Vec::new(),
        }),
        None,
    );
    let original = envelope(
        2,
        "trace.correction",
        ObservabilityPayloadV1::WorkConflictOutcome(WorkConflictOutcomeLinkedV1 {
            prediction_ref: "prediction.correction".to_owned(),
            kind: ConflictKindV1::Mechanical,
            outcome: ConflictOutcomeV1::Conflict,
            adjudicator: ConflictAdjudicatorV1::NativeGit,
            horizon_micros: 500,
            coverage: CoverageStateV1::Known,
            correction_revision: 1,
        }),
        None,
    );
    let corrected = envelope(
        3,
        "trace.correction",
        ObservabilityPayloadV1::WorkConflictOutcome(WorkConflictOutcomeLinkedV1 {
            prediction_ref: "prediction.correction".to_owned(),
            kind: ConflictKindV1::Mechanical,
            outcome: ConflictOutcomeV1::NoConflict,
            adjudicator: ConflictAdjudicatorV1::NativeGit,
            horizon_micros: 500,
            coverage: CoverageStateV1::Known,
            correction_revision: 2,
        }),
        None,
    );
    let forward = read(&Observations::Page(page(vec![
        prediction.clone(),
        original.clone(),
        corrected.clone(),
    ])))
    .await;
    let reverse = read(&Observations::Page(page(vec![
        corrected, original, prediction,
    ])))
    .await;

    assert_eq!(forward.measurements, reverse.measurements);
    let corrected_total = find(
        &forward,
        "work_conflict_prediction_total",
        &[
            ExecutionTopologyDimensionV1::ConflictKind(ExecutionConflictKindV1::Mechanical),
            ExecutionTopologyDimensionV1::ConflictOutcome(ExecutionConflictOutcomeV1::NoConflict),
        ],
    );
    assert_eq!(corrected_total.value.value, None);
    assert_eq!(
        corrected_total.unavailable,
        Some(ExecutionMetricUnavailableV1::SupportFloorUnmet)
    );
}

#[tokio::test]
async fn an_unreadable_store_and_a_capped_page_are_distinct_typed_absences() {
    let refused = read(&Observations::Refused).await;
    assert!(!refused.current);
    assert!(!refused.measurements.is_empty());
    for measurement in &refused.measurements {
        assert_eq!(
            measurement.unavailable,
            Some(ExecutionMetricUnavailableV1::StoreUnavailable)
        );
        assert_eq!(measurement.value.value, None);
    }

    let mut capped = page(vec![envelope(
        1,
        "trace.a",
        topology_sample(4, 4, 2, 1, Vec::new()),
        Some((0, 1_000_000)),
    )]);
    capped.next_watermark = Some("watermark.next".to_owned());
    let capped = read(&Observations::Page(capped)).await;
    for measurement in &capped.measurements {
        assert_eq!(
            measurement.unavailable,
            Some(ExecutionMetricUnavailableV1::EventBudgetExceeded)
        );
    }
}

#[tokio::test]
async fn no_metric_label_or_read_model_field_carries_an_identity() {
    let fanout = ObservabilityPayloadV1::WorkDeliveryFanout(WorkDeliveryFanoutObservedV1 {
        event_class: DeliveryEventClassV1::OperationTerminal,
        surface: DeliverySurfaceFamilyV1::Mcp,
        eligible: 4,
        attempted: 4,
        delivered: 3,
        deduplicated: 1,
        dropped: 0,
        unknown: 0,
    });
    let model = read(&Observations::Page(page(vec![
        envelope(
            1,
            "trace.secret.identity",
            topology_sample(2, 2, 2, 1, vec!["anchor.secret.identity".to_owned()]),
            Some((0, 1_000_000)),
        ),
        envelope(2, "trace.secret.identity", fanout, None),
    ])))
    .await;

    let rendered = serde_json::to_string(&model).expect("the read model serializes");
    assert!(
        !rendered.contains("secret"),
        "an authorized local join reference or anchor leaked into the read model"
    );
    assert!(!rendered.contains("scope.fixture"));
    assert_eq!(model.drill_anchors.len(), 2);
    assert_eq!(model.drill_anchors[0].cursor, "cursor.1");
}

#[tokio::test]
async fn an_inverted_horizon_and_an_oversized_budget_are_typed_invalid_requests() {
    let observations = Observations::Page(page(Vec::new()));
    let inverted = ExecutionTopologyMetricsRequestV1 {
        horizon: ObservabilityHorizonV1 {
            since_micros: 100,
            until_micros: 100,
        },
        max_events: 10,
    };
    let problem =
        execution_topology_rollup_metrics(&NeverRollupPort, &observations, &context(), &inverted)
            .await
            .expect_err("an inverted horizon is refused before any read");
    assert!(matches!(problem, ApplicationProblem::InvalidRequest { .. }));

    let empty = ExecutionTopologyMetricsRequestV1 {
        horizon: ObservabilityHorizonV1 {
            since_micros: 0,
            until_micros: 100,
        },
        max_events: 0,
    };
    let problem =
        execution_topology_rollup_metrics(&NeverRollupPort, &observations, &context(), &empty)
            .await
            .expect_err("an empty event budget is refused before any read");
    assert!(matches!(problem, ApplicationProblem::InvalidRequest { .. }));

    let above_production_limit = ExecutionTopologyMetricsRequestV1 {
        horizon: ObservabilityHorizonV1 {
            since_micros: 0,
            until_micros: 100,
        },
        max_events: MAX_EXECUTION_TOPOLOGY_EVENTS_V1 + 1,
    };
    let problem = execution_topology_rollup_metrics(
        &NeverRollupPort,
        &observations,
        &context(),
        &above_production_limit,
    )
    .await
    .expect_err("the core cannot advertise more rows than production can return");
    assert!(matches!(problem, ApplicationProblem::InvalidRequest { .. }));
}

#[tokio::test]
async fn malformed_store_cursor_authority_is_unavailable_not_an_unanchored_success() {
    let mut malformed = page(vec![envelope(
        1,
        "trace.a",
        topology_sample(2, 2, 1, 1, Vec::new()),
        Some((0, 1_000_000)),
    )]);
    malformed.event_cursors.clear();

    let model = read(&Observations::Page(malformed)).await;
    assert!(model.drill_anchors.is_empty());
    assert!(model.measurements.iter().all(|measurement| {
        measurement.unavailable == Some(ExecutionMetricUnavailableV1::StoreUnavailable)
    }));
}

#[tokio::test]
async fn a_store_row_from_another_scope_cannot_contribute_to_the_authorized_projection() {
    let mut foreign = envelope(
        1,
        "trace.foreign",
        topology_sample(2, 2, 1, 1, Vec::new()),
        Some((0, 1_000_000)),
    );
    foreign.scope_ref =
        "sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff".to_owned();
    let model = read(&Observations::Page(page(vec![foreign]))).await;

    assert_eq!(model.coverage.observed, 0);
    assert_eq!(model.coverage.unknown, 1);
    assert_eq!(model.coverage.state, CoverageStateV1::Partial);
    assert!(model.drill_anchors.is_empty());
    assert!(
        model
            .measurements
            .iter()
            .all(|measurement| measurement.value.value.is_none())
    );
}
