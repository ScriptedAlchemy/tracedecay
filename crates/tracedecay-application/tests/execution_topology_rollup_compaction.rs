use tracedecay_application::{
    ExecutionTopologyRollupFragmentV1, ExecutionTopologyRollupRetentionV1, ObservabilityHorizonV1,
    ObservabilityPageV1, build_execution_topology_rollup_fragment,
    check_execution_topology_rollup_retention_json, project_execution_topology_fragments,
};
use tracedecay_domain::{
    BlockedCauseV1, ConflictAdjudicatorV1, ConflictKindV1, ConflictOutcomeV1, ConflictPredictionV1,
    ConflictScoreKindV1, CoverageStateV1, DuplicateEffectOutcomeV1, DuplicateEffortKindV1,
    LeakOwnerClassV1, ObservabilityEnvelopeV1, ObservabilityPayloadV1,
    ObservabilityRetentionClassV1, QuantityEvidenceClassV1, WorkBlockedIntervalObservedV1,
    WorkConflictOutcomeLinkedV1, WorkDuplicateEffortObservedV1, WorkExecutionLeakKindV1,
    WorkExecutionLeakObservedV1, WorkExecutionLeakRecoveryV1,
};

const DAY_MICROS: i64 = 86_400_000_000;
const SCOPE: &str = "project.execution-topology-rollup-compaction";

fn horizon(since_micros: i64, until_micros: i64) -> ObservabilityHorizonV1 {
    ObservabilityHorizonV1 {
        since_micros,
        until_micros,
    }
}

fn page(events: Vec<ObservabilityEnvelopeV1>, watermark: &str) -> ObservabilityPageV1 {
    let event_cursors = events
        .iter()
        .map(|event| format!("cursor.{}", event.event_id))
        .collect();
    ObservabilityPageV1 {
        events,
        event_cursors,
        watermark: watermark.to_owned(),
        coverage: CoverageStateV1::Known,
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
) -> ObservabilityEnvelopeV1 {
    let envelope = ObservabilityEnvelopeV1 {
        event_id: format!("event.rollup-compaction.{sequence}"),
        event_kind: payload.event_kind().to_owned(),
        schema_revision: 1,
        idempotency_key: format!("idempotency.rollup-compaction.{sequence}"),
        trace_id: trace_id.to_owned(),
        scope_ref: SCOPE.to_owned(),
        capability: "capability.work".to_owned(),
        operation: "operation.execution-topology-rollup-compaction.fixture".to_owned(),
        event_time_micros,
        observation_time_micros: event_time_micros.saturating_add(1),
        valid_from_micros,
        valid_until_micros,
        quantity: None,
        unit: None,
        terminal_result: None,
        producer_revision: "producer.execution-topology-rollup-compaction.v1".to_owned(),
        configuration_revision: "configuration.execution-topology-rollup-compaction.v1".to_owned(),
        policy_revision: "policy.execution-topology-rollup-compaction.v1".to_owned(),
        watermark: format!("event-watermark.rollup-compaction.{sequence}"),
        coverage: CoverageStateV1::Known,
        sampling_probability: None,
        retention_class: ObservabilityRetentionClassV1::LocalRollup395d,
        emitted_count: 1,
        delayed_count: 0,
        dropped_count: 0,
        process_boot_id: "boot.rollup-compaction".to_owned(),
        producer_sequence: sequence,
        payload,
    };
    envelope.validate().unwrap();
    envelope
}

fn duplicate_event(
    sequence: u64,
    event_time_micros: i64,
    adjudication_revision: u64,
) -> ObservabilityEnvelopeV1 {
    envelope(
        sequence,
        event_time_micros,
        "trace.rollup-compaction-duplicate",
        ObservabilityPayloadV1::WorkDuplicateEffort(WorkDuplicateEffortObservedV1 {
            adjudication_ref: "duplicate.rollup-compaction".to_owned(),
            adjudication_revision,
            kind: DuplicateEffortKindV1::ExactDuplicate,
            wall_micros: Some(1),
            token_count: None,
            cost_micros: None,
            test_count: None,
            effect_count: None,
            evidence: QuantityEvidenceClassV1::LocallyMeasured,
            effect_outcome: DuplicateEffectOutcomeV1::Committed,
            coverage: CoverageStateV1::Known,
            local_anchor_refs: Vec::new(),
        }),
        None,
        None,
    )
}

fn leak_event(
    sequence: u64,
    event_time_micros: i64,
    adjudication_revision: u64,
) -> ObservabilityEnvelopeV1 {
    envelope(
        sequence,
        event_time_micros,
        "trace.rollup-compaction-leak",
        ObservabilityPayloadV1::WorkExecutionLeak(WorkExecutionLeakObservedV1 {
            adjudication_ref: "leak.rollup-compaction".to_owned(),
            adjudication_revision,
            kind: WorkExecutionLeakKindV1::AttemptWithoutLiveOwner,
            detection_horizon_micros: 1_000,
            recovery: WorkExecutionLeakRecoveryV1::Pending,
            owner_class: LeakOwnerClassV1::Work,
            coverage: CoverageStateV1::Known,
        }),
        None,
        None,
    )
}

fn blocked_event(
    sequence: u64,
    event_time_micros: i64,
    interval_revision: u32,
) -> ObservabilityEnvelopeV1 {
    envelope(
        sequence,
        event_time_micros,
        "trace.rollup-compaction-blocked",
        ObservabilityPayloadV1::WorkBlockedInterval(WorkBlockedIntervalObservedV1 {
            cause: BlockedCauseV1::Dependency,
            interval_revision,
            valid_from_micros: 1_000,
            valid_until_micros: Some(2_000),
            coverage: CoverageStateV1::Known,
        }),
        None,
        None,
    )
}

fn conflict_outcome_event(sequence: u64, event_time_micros: i64) -> ObservabilityEnvelopeV1 {
    envelope(
        sequence,
        event_time_micros,
        "trace.rollup-compaction-conflict",
        ObservabilityPayloadV1::WorkConflictOutcome(WorkConflictOutcomeLinkedV1 {
            prediction_ref: "prediction.rollup-compaction".to_owned(),
            kind: ConflictKindV1::Mechanical,
            outcome: ConflictOutcomeV1::NoConflict,
            adjudicator: ConflictAdjudicatorV1::NativeGit,
            horizon_micros: 1_000,
            coverage: CoverageStateV1::Known,
            correction_revision: 2,
        }),
        None,
        None,
    )
}

fn conflict_prediction_event(sequence: u64, event_time_micros: i64) -> ObservabilityEnvelopeV1 {
    envelope(
        sequence,
        event_time_micros,
        "trace.rollup-compaction-conflict",
        ObservabilityPayloadV1::WorkConflictPrediction(
            tracedecay_domain::WorkConflictPredictionObservedV1 {
                prediction_ref: "prediction.rollup-compaction".to_owned(),
                kind: ConflictKindV1::Mechanical,
                prediction: ConflictPredictionV1::Conflict,
                score_kind: ConflictScoreKindV1::Rule,
                descriptor_revision: "conflict-descriptor.v1".to_owned(),
                calibration_revision: "conflict-calibration.v1".to_owned(),
                eligible_relation_count: 1,
                expires_at_micros: event_time_micros.saturating_add(DAY_MICROS),
                coverage: CoverageStateV1::Known,
                local_anchor_refs: Vec::new(),
            },
        ),
        None,
        None,
    )
}

fn compact_json(fragment: &ExecutionTopologyRollupFragmentV1, now_micros: i64) -> String {
    let source = serde_json::to_string(fragment).unwrap();
    match check_execution_topology_rollup_retention_json(&source, now_micros).unwrap() {
        ExecutionTopologyRollupRetentionV1::Updated { fragment_json } => fragment_json,
        ExecutionTopologyRollupRetentionV1::Unchanged => {
            panic!("the first retention pass must change the canonical fragment")
        }
    }
}

fn decode(fragment_json: &str) -> ExecutionTopologyRollupFragmentV1 {
    serde_json::from_str(fragment_json).unwrap()
}

#[test]
fn post_retention_corrections_join_retained_bounded_evidence_exactly() {
    let day0 = horizon(0, DAY_MICROS);
    let day1 = horizon(DAY_MICROS, DAY_MICROS.saturating_mul(2));
    let base = build_execution_topology_rollup_fragment(
        SCOPE,
        &day0,
        10,
        page(
            vec![
                duplicate_event(1, 1_000_000, 1),
                leak_event(2, 2_000_000, 1),
                blocked_event(3, 3_000_000, 1),
                conflict_prediction_event(4, 4_000_000),
            ],
            "base-before-compaction",
        ),
    )
    .unwrap();
    let compacted = decode(&compact_json(&base, 31 * DAY_MICROS));
    let late_events = [
        duplicate_event(11, DAY_MICROS + 1_000_000, 2),
        leak_event(12, DAY_MICROS + 2_000_000, 2),
        blocked_event(13, DAY_MICROS + 3_000_000, 2),
        conflict_outcome_event(14, DAY_MICROS + 4_000_000),
    ];
    for (index, event) in late_events.into_iter().enumerate() {
        let late = build_execution_topology_rollup_fragment(
            SCOPE,
            &day1,
            20 + index as i64,
            page(vec![event], &format!("late-correction-{index}")),
        )
        .unwrap();
        let model = project_execution_topology_fragments(
            SCOPE,
            &horizon(0, DAY_MICROS.saturating_mul(2)),
            30 + index as i64,
            &[compacted.clone(), late],
        );
        assert!(model.current);
        assert_eq!(model.coverage.state, CoverageStateV1::Known);
        assert_ne!(model.watermark, "execution-topology:rollup-unavailable");
    }
}
