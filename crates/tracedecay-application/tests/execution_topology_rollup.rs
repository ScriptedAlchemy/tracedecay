use tracedecay_application::{
    CancellationContext, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    ExecutionConflictKindV1, ExecutionConflictOutcomeV1, ExecutionLeakKindV1,
    ExecutionLeakOutcomeV1, ExecutionMetricUnavailableV1, ExecutionTopologyDimensionV1,
    ExecutionTopologyMetricsRequestV1, ExecutionTopologyMetricsV1,
    ExecutionTopologyRollupFragmentPageV1, ExecutionTopologyRollupQueryPort,
    MAX_EXECUTION_TOPOLOGY_ROLLUP_FRAGMENT_BYTES_V1, ObservabilityFuture, ObservabilityHorizonV1,
    ObservabilityPageV1, ObservabilityQueryPort, ObservabilityQueryV1, RequestContext, RequestId,
    ResolvedScope, build_empty_execution_topology_daily_rollup,
    build_execution_topology_boundary_fragment, build_execution_topology_daily_rollup,
    build_execution_topology_rollup_fragment, execution_topology_rollup_metrics,
    project_execution_topology_fragments, project_execution_topology_fragments_with_boundaries,
};
use tracedecay_domain::{
    ActorId, BlockedCauseV1, ConflictAdjudicatorV1, ConflictKindV1, ConflictOutcomeV1,
    ConflictPredictionV1, ConflictScoreKindV1, CoverageStateV1, ExecutionPlacementV1,
    ExecutionTopologyKindV1, ExecutionTopologySampledV1, IntegrationOperationKindV1,
    IntegrationOwnerReceiptV1, IntegrationPhaseV1, IntegrationResultV1, IntegrationScopeClassV1,
    LeakOwnerClassV1, ManifestDigest, ObservabilityEnvelopeV1, ObservabilityPayloadV1,
    ObservabilityRetentionClassV1, ProjectId, RepositoryId, ReviewTopologyV1,
    TelemetryDropObservedV1, UtcMicros, WorkBlockedIntervalObservedV1, WorkConflictOutcomeLinkedV1,
    WorkConflictPredictionObservedV1, WorkExecutionLeakKindV1, WorkExecutionLeakObservedV1,
    WorkExecutionLeakRecoveryV1, WorkIntegrationTransitionObservedV1, WorkTopologyBranchV1,
    WorktreeId,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};
#[path = "execution_topology_rollup/stack_drift.rs"]
mod stack_drift;

const DAY_MICROS: i64 = 86_400_000_000;
const SCOPE: &str = "project.execution-topology-rollup";
fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}
fn rollup_context() -> RequestContext {
    let scope = ResolvedScope::new(
        id::<ProjectId>(SCOPE),
        id::<RepositoryId>("repository.execution-topology-rollup"),
        id::<WorktreeId>("worktree.execution-topology-rollup"),
        None,
    )
    .unwrap();
    let capability = CapabilityId::new("capability.work.topology_metrics").unwrap();
    let use_case = UseCaseId::new("use-case.work.topology_metrics").unwrap();
    let grant = CapabilityGrantSnapshot::new(
        id("grant.execution-topology-rollup"),
        1,
        ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
        id::<ActorId>("actor.execution-topology-rollup-issuer"),
        UtcMicros(1),
        UtcMicros(i64::MAX),
        scope.clone(),
        std::collections::BTreeSet::from([capability]),
        std::collections::BTreeSet::from([use_case]),
        DisclosureClass::Sensitive,
    )
    .unwrap();
    RequestContext::new(
        id::<ActorId>("actor.execution-topology-rollup-reader"),
        scope,
        grant,
        RequestId::new("request.execution-topology-rollup").unwrap(),
        Deadline::new(UtcMicros(i64::MAX)).unwrap(),
        CancellationContext::active("cancel.execution-topology-rollup").unwrap(),
    )
    .unwrap()
}
#[derive(Clone)]
struct StaticRollupPort {
    page: ExecutionTopologyRollupFragmentPageV1,
}

impl ExecutionTopologyRollupQueryPort for StaticRollupPort {
    fn query_rollup_fragments<'a>(
        &'a self,
        _query: tracedecay_application::ExecutionTopologyRollupFragmentQueryV1,
    ) -> ObservabilityFuture<'a, ExecutionTopologyRollupFragmentPageV1> {
        let page = self.page.clone();
        Box::pin(async move { Ok(page) })
    }
}
struct EmptyObservations;

impl ObservabilityQueryPort for EmptyObservations {
    fn query<'a>(
        &'a self,
        _query: ObservabilityQueryV1,
    ) -> ObservabilityFuture<'a, ObservabilityPageV1> {
        Box::pin(async {
            Ok(ObservabilityPageV1 {
                events: Vec::new(),
                event_cursors: Vec::new(),
                watermark: "empty-boundary".to_owned(),
                coverage: CoverageStateV1::Known,
                next_watermark: None,
            })
        })
    }
}
async fn read_rollup_page(
    requested_horizon: ObservabilityHorizonV1,
    page: ExecutionTopologyRollupFragmentPageV1,
) -> ExecutionTopologyMetricsV1 {
    let request = ExecutionTopologyMetricsRequestV1 {
        horizon: requested_horizon,
        max_events: 1_000,
    };
    execution_topology_rollup_metrics(
        &StaticRollupPort { page },
        &EmptyObservations,
        &rollup_context(),
        &request,
    )
    .await
    .expect("authorized retained rollup read returns a typed model")
}
fn assert_read_coverage(
    model: &ExecutionTopologyMetricsV1,
    expected_state: CoverageStateV1,
    expected_reason: ExecutionMetricUnavailableV1,
) {
    assert!(!model.current);
    assert_eq!(model.coverage.state, expected_state);
    assert_eq!(model.github_stack_capability.coverage.state, expected_state);
    assert_eq!(
        model.github_stack_capability.unavailable,
        Some(expected_reason)
    );
    assert!(model.measurements.iter().all(|measurement| {
        measurement.value.coverage.state == expected_state
            && measurement.value.value.is_none()
            && measurement.unavailable == Some(expected_reason)
    }));
}
fn horizon(since_micros: i64, until_micros: i64) -> ObservabilityHorizonV1 {
    ObservabilityHorizonV1 {
        since_micros,
        until_micros,
    }
}
fn page(
    events: Vec<ObservabilityEnvelopeV1>,
    watermark: &str,
    coverage: CoverageStateV1,
) -> ObservabilityPageV1 {
    let event_cursors = events
        .iter()
        .map(|event| format!("cursor.{}", event.event_id))
        .collect();
    ObservabilityPageV1 {
        events,
        event_cursors,
        watermark: watermark.to_owned(),
        coverage,
        next_watermark: None,
    }
}
fn envelope(
    sequence: u64,
    event_time_micros: i64,
    trace_id: &str,
    payload: ObservabilityPayloadV1,
    valid_from_micros: Option<i64>,
    valid_until_micros: Option<i64>,
    coverage: CoverageStateV1,
    dropped_count: u64,
    process_boot_id: &str,
) -> ObservabilityEnvelopeV1 {
    let envelope = ObservabilityEnvelopeV1 {
        event_id: format!("event.rollup.{sequence}"),
        event_kind: payload.event_kind().to_owned(),
        schema_revision: 1,
        idempotency_key: format!("idempotency.rollup.{sequence}"),
        trace_id: trace_id.to_owned(),
        scope_ref: SCOPE.to_owned(),
        capability: "capability.work".to_owned(),
        operation: "operation.execution-topology-rollup.fixture".to_owned(),
        event_time_micros,
        observation_time_micros: event_time_micros.saturating_add(1),
        valid_from_micros,
        valid_until_micros,
        quantity: None,
        unit: None,
        terminal_result: None,
        producer_revision: "producer.execution-topology-rollup.v1".to_owned(),
        configuration_revision: "configuration.execution-topology-rollup.v1".to_owned(),
        policy_revision: "policy.execution-topology-rollup.v1".to_owned(),
        watermark: format!("event-watermark.rollup.{sequence}"),
        coverage,
        sampling_probability: None,
        retention_class: ObservabilityRetentionClassV1::LocalRollup395d,
        emitted_count: 1,
        delayed_count: 0,
        dropped_count,
        process_boot_id: process_boot_id.to_owned(),
        producer_sequence: sequence,
        payload,
    };
    envelope
        .validate()
        .expect("rollup fixture envelope satisfies the domain contract");
    envelope
}
fn topology_event(sequence: u64, event_time_micros: i64) -> ObservabilityEnvelopeV1 {
    topology_event_with(
        sequence,
        event_time_micros,
        4,
        CoverageStateV1::Known,
        0,
        "boot.topology-rollup",
    )
}
fn topology_event_with(
    sequence: u64,
    event_time_micros: i64,
    active_width: u16,
    coverage: CoverageStateV1,
    dropped_count: u64,
    process_boot_id: &str,
) -> ObservabilityEnvelopeV1 {
    envelope(
        sequence,
        event_time_micros,
        &format!("trace.topology.{sequence}"),
        ObservabilityPayloadV1::ExecutionTopology(ExecutionTopologySampledV1 {
            topology: ExecutionTopologyKindV1::Parallel,
            placement: ExecutionPlacementV1::LinkedWorktree,
            branch_topology: WorkTopologyBranchV1::IndependentBranches,
            review_topology: ReviewTopologyV1::IndependentReview,
            integration_strategy: tracedecay_domain::IntegrationStrategyV1::FastForwardOnly,
            requested_width: active_width,
            accepted_width: active_width,
            admitted_width: active_width,
            active_width,
            useful_width: 1,
            runnable_count: active_width,
            blocked_count: 0,
            shared_authority_serialized_count: 0,
            local_anchor_refs: Vec::new(),
        }),
        Some(event_time_micros),
        Some(event_time_micros.saturating_add(1_000)),
        coverage,
        dropped_count,
        process_boot_id,
    )
}
fn integration_event(
    sequence: u64,
    event_time_micros: i64,
    trace_id: &str,
    phase: IntegrationPhaseV1,
    result: IntegrationResultV1,
    operation: IntegrationOperationKindV1,
    valid_from_micros: Option<i64>,
) -> ObservabilityEnvelopeV1 {
    let owner_receipt = if phase == IntegrationPhaseV1::NativeIntegratedObserved {
        IntegrationOwnerReceiptV1::NativeGitObservation
    } else {
        IntegrationOwnerReceiptV1::None
    };
    envelope(
        sequence,
        event_time_micros,
        trace_id,
        ObservabilityPayloadV1::WorkIntegrationTransition(WorkIntegrationTransitionObservedV1 {
            phase,
            result,
            operation,
            source_scope: IntegrationScopeClassV1::Repository,
            target_scope: IntegrationScopeClassV1::Repository,
            dependency_commits_eligible: 0,
            dependency_commits_observed: 0,
            required_checks_eligible: 0,
            required_checks_observed: 0,
            owner_receipt,
            coverage: CoverageStateV1::Known,
            local_anchor_refs: Vec::new(),
        }),
        valid_from_micros,
        None,
        CoverageStateV1::Known,
        0,
        "boot.integration-rollup",
    )
}
fn conflict_prediction(
    sequence: u64,
    event_time_micros: i64,
    reference: &str,
) -> ObservabilityEnvelopeV1 {
    conflict_prediction_with(
        sequence,
        event_time_micros,
        reference,
        ConflictPredictionV1::Conflict,
    )
}
fn conflict_prediction_with(
    sequence: u64,
    event_time_micros: i64,
    reference: &str,
    prediction: ConflictPredictionV1,
) -> ObservabilityEnvelopeV1 {
    envelope(
        sequence,
        event_time_micros,
        &format!("trace.conflict.{reference}"),
        ObservabilityPayloadV1::WorkConflictPrediction(WorkConflictPredictionObservedV1 {
            prediction_ref: reference.to_owned(),
            kind: ConflictKindV1::Mechanical,
            prediction,
            score_kind: ConflictScoreKindV1::Rule,
            descriptor_revision: "conflict-descriptor.v1".to_owned(),
            calibration_revision: "conflict-calibration.v1".to_owned(),
            eligible_relation_count: 1,
            expires_at_micros: event_time_micros.saturating_add(DAY_MICROS),
            coverage: CoverageStateV1::Known,
            local_anchor_refs: Vec::new(),
        }),
        None,
        None,
        CoverageStateV1::Known,
        0,
        "boot.correction-rollup",
    )
}
fn conflict_outcome(
    sequence: u64,
    event_time_micros: i64,
    reference: &str,
    outcome: ConflictOutcomeV1,
    correction_revision: u32,
) -> ObservabilityEnvelopeV1 {
    envelope(
        sequence,
        event_time_micros,
        &format!("trace.conflict.{reference}"),
        ObservabilityPayloadV1::WorkConflictOutcome(WorkConflictOutcomeLinkedV1 {
            prediction_ref: reference.to_owned(),
            kind: ConflictKindV1::Mechanical,
            outcome,
            adjudicator: ConflictAdjudicatorV1::NativeGit,
            horizon_micros: 1_000,
            coverage: CoverageStateV1::Known,
            correction_revision,
        }),
        None,
        None,
        CoverageStateV1::Known,
        0,
        "boot.correction-rollup",
    )
}
fn leak_event(
    sequence: u64,
    event_time_micros: i64,
    reference: &str,
    recovery: WorkExecutionLeakRecoveryV1,
    revision: u64,
) -> ObservabilityEnvelopeV1 {
    envelope(
        sequence,
        event_time_micros,
        &format!("trace.leak.{reference}"),
        ObservabilityPayloadV1::WorkExecutionLeak(WorkExecutionLeakObservedV1 {
            adjudication_ref: reference.to_owned(),
            adjudication_revision: revision,
            kind: WorkExecutionLeakKindV1::AttemptWithoutLiveOwner,
            detection_horizon_micros: 1_000,
            recovery,
            owner_class: LeakOwnerClassV1::Work,
            coverage: CoverageStateV1::Known,
        }),
        None,
        None,
        CoverageStateV1::Known,
        0,
        "boot.correction-rollup",
    )
}
fn blocked_event(
    sequence: u64,
    event_time_micros: i64,
    trace_id: &str,
    revision: u32,
    from_micros: i64,
    until_micros: i64,
) -> ObservabilityEnvelopeV1 {
    envelope(
        sequence,
        event_time_micros,
        trace_id,
        ObservabilityPayloadV1::WorkBlockedInterval(WorkBlockedIntervalObservedV1 {
            cause: BlockedCauseV1::Dependency,
            interval_revision: revision,
            valid_from_micros: from_micros,
            valid_until_micros: Some(until_micros),
            coverage: CoverageStateV1::Known,
        }),
        None,
        None,
        CoverageStateV1::Known,
        0,
        "boot.correction-rollup",
    )
}
fn drop_receipt(sequence: u64, event_time_micros: i64) -> ObservabilityEnvelopeV1 {
    envelope(
        sequence,
        event_time_micros,
        "trace.drop.cross-boundary",
        ObservabilityPayloadV1::TelemetryDrop(TelemetryDropObservedV1 {
            first_missing_sequence: 1,
            last_missing_sequence: 5,
            proved_drop_lower_bound: 5,
            clean_shutdown_observed: false,
        }),
        None,
        None,
        CoverageStateV1::Known,
        0,
        "boot.drop-cross-boundary",
    )
}
fn assert_equivalent(raw: &ExecutionTopologyMetricsV1, rollup: &ExecutionTopologyMetricsV1) {
    let mut raw = raw.clone();
    let mut rollup = rollup.clone();
    raw.observed_at_micros = 0;
    rollup.observed_at_micros = 0;
    raw.watermark = "normalized-watermark".to_owned();
    rollup.watermark = "normalized-watermark".to_owned();
    raw.drill_anchors.clear();
    rollup.drill_anchors.clear();
    for model in [&mut raw, &mut rollup] {
        for measurement in &mut model.measurements {
            measurement.value.provenance.watermark = "normalized-watermark".to_owned();
        }
    }
    assert_eq!(raw, rollup);
}
fn find<'a>(
    model: &'a ExecutionTopologyMetricsV1,
    metric: &str,
    dimensions: &[ExecutionTopologyDimensionV1],
) -> &'a tracedecay_application::ExecutionTopologyMeasurementV1 {
    model
        .measurements
        .iter()
        .find(|measurement| {
            measurement.value.metric == metric && measurement.dimensions == dimensions
        })
        .unwrap_or_else(|| panic!("descriptor {metric} has the requested dimensions"))
}
fn assert_store_unavailable(model: &ExecutionTopologyMetricsV1) {
    assert!(!model.current);
    assert_eq!(model.watermark, "execution-topology:rollup-unavailable");
    assert!(model.measurements.iter().all(|measurement| {
        measurement.unavailable == Some(ExecutionMetricUnavailableV1::StoreUnavailable)
            && measurement.value.value.is_none()
    }));
}
#[test]
fn late_conflict_leak_and_blocked_corrections_choose_highest_revision_across_days() {
    let requested = horizon(0, DAY_MICROS.saturating_mul(2));
    let day0 = horizon(0, DAY_MICROS);
    let day1 = horizon(DAY_MICROS, DAY_MICROS.saturating_mul(2));
    let mut first_day = Vec::new();
    let mut second_day = Vec::new();
    for index in 0..6_u64 {
        let reference = format!("prediction.correction.{index}");
        first_day.push(conflict_prediction(
            index + 1,
            1_000_000 + index as i64,
            &reference,
        ));
        if index == 0 {
            first_day.push(conflict_outcome(
                20 + index,
                2_000_000,
                &reference,
                ConflictOutcomeV1::Conflict,
                1,
            ));
        } else {
            second_day.push(conflict_outcome(
                200 + index,
                DAY_MICROS + 2_000_000 + index as i64,
                &reference,
                ConflictOutcomeV1::Conflict,
                1,
            ));
        }
        let leak_reference = format!("leak.correction.{index}");
        first_day.push(leak_event(
            40 + index,
            3_000_000 + index as i64,
            &leak_reference,
            WorkExecutionLeakRecoveryV1::Pending,
            1,
        ));
        if index == 0 {
            second_day.push(leak_event(
                240 + index,
                DAY_MICROS + 3_000_000,
                &leak_reference,
                WorkExecutionLeakRecoveryV1::Recovered,
                2,
            ));
        }
        let blocked_trace = format!("trace.blocked.correction.{index}");
        let from = 1_000_000 + index as i64 * 10_000_000;
        first_day.push(blocked_event(
            60 + index,
            4_000_000 + index as i64,
            &blocked_trace,
            1,
            from,
            from + 1_000_000,
        ));
        second_day.push(blocked_event(
            260 + index,
            DAY_MICROS + 4_000_000 + index as i64,
            &blocked_trace,
            2,
            from,
            from + 2_000_000,
        ));
    }
    second_day.push(conflict_outcome(
        300,
        DAY_MICROS + 2_000_001,
        "prediction.correction.0",
        ConflictOutcomeV1::NoConflict,
        2,
    ));
    let first = build_execution_topology_rollup_fragment(
        SCOPE,
        &day0,
        202,
        page(first_day, "correction-day-0", CoverageStateV1::Known),
    )
    .expect("corrected day zero is bounded");
    let second = build_execution_topology_rollup_fragment(
        SCOPE,
        &day1,
        203,
        page(second_day, "correction-day-1", CoverageStateV1::Known),
    )
    .expect("corrected day one is bounded");
    let merged = project_execution_topology_fragments(SCOPE, &requested, 204, &[first, second]);
    let conflict = |outcome| {
        find(
            &merged,
            "work_conflict_prediction_total",
            &[
                ExecutionTopologyDimensionV1::ConflictKind(ExecutionConflictKindV1::Mechanical),
                ExecutionTopologyDimensionV1::ConflictOutcome(outcome),
            ],
        )
        .value
        .value
    };
    assert_eq!(conflict(ExecutionConflictOutcomeV1::Conflict), Some(5.0));
    assert_eq!(conflict(ExecutionConflictOutcomeV1::NoConflict), None);
    let leak = |outcome| {
        find(
            &merged,
            "work_execution_leaks_total",
            &[
                ExecutionTopologyDimensionV1::LeakKind(
                    ExecutionLeakKindV1::AttemptWithoutLiveOwner,
                ),
                ExecutionTopologyDimensionV1::LeakOutcome(outcome),
            ],
        )
        .value
        .value
    };
    assert_eq!(leak(ExecutionLeakOutcomeV1::Pending), Some(5.0));
    assert_eq!(leak(ExecutionLeakOutcomeV1::Recovered), None);
    let blocked = find(&merged, "work_blocked_wall_seconds", &[]);
    assert_eq!(blocked.value.value, Some(12.0));
    assert_eq!(blocked.value.coverage.eligible, Some(6));
}
#[test]
fn daily_fragment_input_order_is_irrelevant_and_drop_receipt_carrier_crosses_boundary_once() {
    let requested = horizon(0, DAY_MICROS.saturating_mul(2));
    let day0 = horizon(0, DAY_MICROS);
    let day1 = horizon(DAY_MICROS, DAY_MICROS.saturating_mul(2));
    let mut first_day = vec![drop_receipt(100, DAY_MICROS - 2_000_000)];
    first_day.extend((0..5_u64).map(|index| {
        topology_event_with(
            101 + index,
            DAY_MICROS - 1_000_000 + index as i64,
            2,
            CoverageStateV1::Known,
            0,
            "boot.drop-cross-boundary",
        )
    }));
    let mut second_day = (0..5_u64)
        .map(|index| topology_event(120 + index, DAY_MICROS + 1_000_000 + index as i64))
        .collect::<Vec<_>>();
    second_day.push(topology_event_with(
        6,
        DAY_MICROS + 2_000_000,
        2,
        CoverageStateV1::Known,
        5,
        "boot.drop-cross-boundary",
    ));
    let first = build_execution_topology_rollup_fragment(
        SCOPE,
        &day0,
        302,
        page(first_day, "drop-day-0", CoverageStateV1::Known),
    )
    .expect("drop receipt day is bounded");
    let second = build_execution_topology_rollup_fragment(
        SCOPE,
        &day1,
        303,
        page(second_day, "drop-day-1", CoverageStateV1::Known),
    )
    .expect("drop carrier day is bounded");
    let forward = project_execution_topology_fragments(
        SCOPE,
        &requested,
        304,
        &[first.clone(), second.clone()],
    );
    let reverse = project_execution_topology_fragments(SCOPE, &requested, 305, &[second, first]);
    assert_equivalent(&forward, &reverse);
    assert_eq!(forward.emission_coverage.dropped, Some(5));
    assert_eq!(forward.coverage.observed, 11);
    assert_eq!(forward.coverage.unknown, 5);
    assert_eq!(forward.coverage.state, CoverageStateV1::Partial);
}
#[test]
fn arbitrary_partial_boundaries_merge_with_full_day_interior_without_changing_projection() {
    let first_boundary_horizon = horizon(DAY_MICROS / 2, DAY_MICROS);
    let interior_horizon = horizon(DAY_MICROS, DAY_MICROS.saturating_mul(2));
    let last_boundary_horizon = horizon(
        DAY_MICROS.saturating_mul(2),
        DAY_MICROS * 2 + DAY_MICROS / 2,
    );
    let requested = horizon(
        first_boundary_horizon.since_micros,
        last_boundary_horizon.until_micros,
    );
    let first_events = (0..5_u64)
        .map(|index| topology_event(400 + index, DAY_MICROS / 2 + 1_000_000 + index as i64))
        .collect::<Vec<_>>();
    let interior_events = (0..5_u64)
        .map(|index| topology_event(410 + index, DAY_MICROS + 1_000_000 + index as i64))
        .collect::<Vec<_>>();
    let last_events = (0..5_u64)
        .map(|index| topology_event(420 + index, DAY_MICROS * 2 + 1_000_000 + index as i64))
        .collect::<Vec<_>>();
    let first = build_execution_topology_boundary_fragment(
        SCOPE,
        &first_boundary_horizon,
        page(first_events, "boundary-first", CoverageStateV1::Known),
    )
    .expect("first nonempty partial UTC day is transient-only");
    let interior = build_execution_topology_rollup_fragment(
        SCOPE,
        &interior_horizon,
        402,
        page(interior_events, "interior-day", CoverageStateV1::Known),
    )
    .expect("interior full UTC day is retained");
    let last = build_execution_topology_boundary_fragment(
        SCOPE,
        &last_boundary_horizon,
        page(last_events, "boundary-last", CoverageStateV1::Known),
    )
    .expect("last nonempty partial UTC day is transient-only");
    let composed = project_execution_topology_fragments_with_boundaries(
        SCOPE,
        &requested,
        403,
        &[interior],
        &[last, first],
    );
    let peak = find(
        &composed,
        "work_execution_fanout_width",
        &[
            tracedecay_application::ExecutionTopologyDimensionV1::FanoutPhase(
                tracedecay_application::ExecutionFanoutPhaseV1::PeakActive,
            ),
            ExecutionTopologyDimensionV1::WidthBucket(
                tracedecay_application::ExecutionWidthBucketV1::From3To4,
            ),
        ],
    );
    assert_eq!(peak.value.value, Some(15.0));
    assert_eq!(peak.value.coverage.eligible, Some(15));
}
#[test]
fn canonical_serde_roundtrip_and_bad_missing_interiors_fail_closed() {
    let exact_day = horizon(0, DAY_MICROS);
    let source_page = page(
        (0..10_000_u64)
            .map(|index| topology_event(500 + index, 1_000_000 + index as i64))
            .collect(),
        "serde-day",
        CoverageStateV1::Known,
    );
    let fragment = build_execution_topology_rollup_fragment(SCOPE, &exact_day, 501, source_page)
        .expect("serde fixture fragment builds");
    let canonical = serde_json::to_string(&fragment).expect("fragment has canonical JSON");
    assert!(canonical.contains("\"state\""));
    assert!(!canonical.contains("\"evidence\""));
    assert!(canonical.len() < 4 * 1024 * 1024);
    let decoded: tracedecay_application::ExecutionTopologyRollupFragmentV1 =
        serde_json::from_str(&canonical).expect("canonical JSON round trips");
    assert_eq!(serde_json::to_string(&decoded).unwrap(), canonical);
    let malformed = format!(
        "{},\"unknown_field\":true}}",
        canonical.strip_suffix('}').unwrap()
    );
    assert!(
        serde_json::from_str::<tracedecay_application::ExecutionTopologyRollupFragmentV1>(
            &malformed
        )
        .is_err()
    );
    let missing = project_execution_topology_fragments(
        SCOPE,
        &horizon(0, DAY_MICROS.saturating_mul(2)),
        502,
        std::slice::from_ref(&fragment),
    );
    assert_store_unavailable(&missing);
}
#[tokio::test]
async fn retained_read_preserves_stale_missing_malformed_and_capped_source_states() {
    let exact_day = horizon(0, DAY_MICROS);
    let full_horizon = horizon(0, DAY_MICROS.saturating_mul(2));
    let fragment = build_execution_topology_rollup_fragment(
        SCOPE,
        &exact_day,
        550,
        page(
            (0..5_u64)
                .map(|index| topology_event(800 + index, 1_000_000 + index as i64))
                .collect(),
            "read-state-day",
            CoverageStateV1::Known,
        ),
    )
    .expect("read-state fixture fragment builds");
    let canonical = serde_json::to_string(&fragment).unwrap();

    let cases = [
        (
            exact_day.clone(),
            ExecutionTopologyRollupFragmentPageV1 {
                horizon: exact_day.clone(),
                coverage: CoverageStateV1::Stale,
                fragment_documents: vec![canonical.clone()],
            },
            CoverageStateV1::Stale,
            ExecutionMetricUnavailableV1::StoreUnavailable,
        ),
        (
            full_horizon.clone(),
            ExecutionTopologyRollupFragmentPageV1 {
                horizon: full_horizon,
                coverage: CoverageStateV1::Known,
                fragment_documents: vec![canonical.clone()],
            },
            CoverageStateV1::Partial,
            ExecutionMetricUnavailableV1::StoreUnavailable,
        ),
        (
            exact_day.clone(),
            ExecutionTopologyRollupFragmentPageV1 {
                horizon: exact_day.clone(),
                coverage: CoverageStateV1::Known,
                fragment_documents: vec!["{not-canonical-json".to_owned()],
            },
            CoverageStateV1::Unknown,
            ExecutionMetricUnavailableV1::StoreUnavailable,
        ),
        (
            exact_day.clone(),
            ExecutionTopologyRollupFragmentPageV1 {
                horizon: exact_day,
                coverage: CoverageStateV1::Known,
                fragment_documents: vec![
                    "x".repeat(MAX_EXECUTION_TOPOLOGY_ROLLUP_FRAGMENT_BYTES_V1 + 1),
                ],
            },
            CoverageStateV1::Capped,
            ExecutionMetricUnavailableV1::EventBudgetExceeded,
        ),
    ];
    for (request_horizon, page, state, reason) in cases {
        let model = read_rollup_page(request_horizon, page).await;
        assert_read_coverage(&model, state, reason);
    }
}
#[test]
fn one_low_support_cell_is_suppressed_without_fabricating_a_value() {
    let day = horizon(0, DAY_MICROS);
    let fragment = build_execution_topology_rollup_fragment(
        SCOPE,
        &day,
        601,
        page(
            vec![topology_event(700, 1_000_000)],
            "low-support",
            CoverageStateV1::Known,
        ),
    )
    .unwrap();
    let model = project_execution_topology_fragments(SCOPE, &day, 602, &[fragment]);
    let cell = find(
        &model,
        "work_execution_fanout_width",
        &[
            tracedecay_application::ExecutionTopologyDimensionV1::FanoutPhase(
                tracedecay_application::ExecutionFanoutPhaseV1::PeakActive,
            ),
            ExecutionTopologyDimensionV1::WidthBucket(
                tracedecay_application::ExecutionWidthBucketV1::From3To4,
            ),
        ],
    );
    assert_eq!(cell.value.value, None);
    assert_eq!(
        cell.unavailable,
        Some(ExecutionMetricUnavailableV1::SupportFloorUnmet)
    );
    assert_eq!(cell.value.coverage.unknown, 1);
}
#[test]
fn conflict_ratios_use_their_exact_local_denominators_for_suppression() {
    let day = horizon(0, DAY_MICROS);
    let mut events = Vec::new();
    for index in 0..50_u64 {
        let reference = format!("prediction.local-support.{index}");
        events.push(conflict_prediction_with(
            2_000 + index,
            1_000_000 + index as i64,
            &reference,
            if index == 0 {
                ConflictPredictionV1::Conflict
            } else {
                ConflictPredictionV1::NoConflict
            },
        ));
        if index < 45 {
            events.push(conflict_outcome(
                3_000 + index,
                2_000_000 + index as i64,
                &reference,
                if index == 0 {
                    ConflictOutcomeV1::Conflict
                } else {
                    ConflictOutcomeV1::NoConflict
                },
                1,
            ));
        }
    }
    let fragment = build_execution_topology_rollup_fragment(
        SCOPE,
        &day,
        603,
        page(events, "conflict-local-support", CoverageStateV1::Known),
    )
    .unwrap();
    let model = project_execution_topology_fragments(SCOPE, &day, 604, &[fragment]);
    for metric in [
        "work_conflict_prediction_precision",
        "work_conflict_prediction_recall",
    ] {
        let cell = find(
            &model,
            metric,
            &[ExecutionTopologyDimensionV1::ConflictKind(
                ExecutionConflictKindV1::Mechanical,
            )],
        );
        assert_eq!(cell.value.value, None);
        assert_eq!(
            cell.unavailable,
            Some(ExecutionMetricUnavailableV1::SupportFloorUnmet)
        );
    }
}
#[test]
fn canonical_fragments_with_impossible_dimensional_state_fail_closed() {
    let day = horizon(0, DAY_MICROS);
    let cases = [
        (
            vec![topology_event(4_000, 1_000_000)],
            "/state/reduced/capacity/topology/useful_attempt_micros",
            serde_json::json!(4_001),
        ),
        (
            vec![integration_event(
                4_001,
                1_000_000,
                "trace.integration.tamper",
                IntegrationPhaseV1::NativeIntegratedObserved,
                IntegrationResultV1::Succeeded,
                IntegrationOperationKindV1::FastForward,
                None,
            )],
            "/state/reduced/lifecycle/merge_totals/0/1/1",
            serde_json::json!(0),
        ),
    ];
    for (events, pointer, replacement) in cases {
        let fragment = build_execution_topology_rollup_fragment(
            SCOPE,
            &day,
            605,
            page(events, "tampered-dimensional-state", CoverageStateV1::Known),
        )
        .unwrap();
        let mut canonical = serde_json::to_value(fragment).unwrap();
        *canonical
            .pointer_mut(pointer)
            .expect("canonical state path") = replacement;
        let tampered = serde_json::from_value(canonical).unwrap();
        assert_store_unavailable(&project_execution_topology_fragments(
            SCOPE,
            &day,
            606,
            &[tampered],
        ));
    }
}
#[test]
fn correction_carry_overflow_is_a_durable_capped_day() {
    let build = build_execution_topology_daily_rollup(
        SCOPE,
        &horizon(0, DAY_MICROS),
        700,
        page(
            (0..513_u64)
                .map(|index| {
                    conflict_prediction(
                        900 + index,
                        1_000_000 + index as i64,
                        &format!("prediction.overflow.{index}"),
                    )
                })
                .collect(),
            "overflow-carry",
            CoverageStateV1::Known,
        ),
    )
    .unwrap();
    assert_eq!(build.coverage, CoverageStateV1::Capped);
    assert!(build.fragment_json.contains("\"kind\":\"capped\""));
}

#[test]
fn empty_known_day_uses_canonical_known_fragment() {
    let build =
        build_empty_execution_topology_daily_rollup(SCOPE, &horizon(0, DAY_MICROS), DAY_MICROS)
            .unwrap();
    assert_eq!(build.coverage, CoverageStateV1::Known);
    assert!(build.fragment_json.contains("\"kind\":\"reduced\""));
    assert_eq!(
        serde_json::to_string(&build.fragment).unwrap(),
        build.fragment_json
    );
}
