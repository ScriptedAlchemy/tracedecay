use super::*;
use tracedecay_domain::{
    DurationBucketV1, IntervalStateV1, StackDriftKindV1, WorkStackDriftObservedV1,
};

#[test]
fn later_closed_drift_replaces_retained_open_state_across_days() {
    let requested = horizon(0, DAY_MICROS.saturating_mul(2));
    let first_day = horizon(0, DAY_MICROS);
    let second_day = horizon(DAY_MICROS, DAY_MICROS.saturating_mul(2));
    let mut open_events = Vec::new();
    let mut closed_events = Vec::new();
    for index in 0..5_u64 {
        let trace = format!("trace.stack-drift.correction.{index}");
        open_events.push(envelope(
            5_000 + index,
            1_000_000 + index as i64,
            &trace,
            ObservabilityPayloadV1::WorkStackDrift(WorkStackDriftObservedV1 {
                kind: StackDriftKindV1::HeadAdvanced,
                state: IntervalStateV1::Open,
                first_observed_micros: 1_000_000 + index as i64,
                terminal_micros: None,
                age_bucket: DurationBucketV1::Under1m,
                coverage: CoverageStateV1::Known,
            }),
            (None, None),
            CoverageStateV1::Known,
            (0, "boot.stack-drift"),
        ));
        closed_events.push(envelope(
            6_000 + index,
            DAY_MICROS + 2_000_000 + index as i64,
            &trace,
            ObservabilityPayloadV1::WorkStackDrift(WorkStackDriftObservedV1 {
                kind: StackDriftKindV1::HeadAdvanced,
                state: IntervalStateV1::Closed,
                first_observed_micros: 1_000_000 + index as i64,
                terminal_micros: Some(DAY_MICROS + 2_000_000 + index as i64),
                age_bucket: DurationBucketV1::From1dTo7d,
                coverage: CoverageStateV1::Known,
            }),
            (None, None),
            CoverageStateV1::Known,
            (0, "boot.stack-drift"),
        ));
    }
    let open = build_execution_topology_rollup_fragment(
        SCOPE,
        &first_day,
        7_000,
        page(open_events, "stack-drift-open", CoverageStateV1::Known),
    )
    .unwrap();
    let closed = build_execution_topology_rollup_fragment(
        SCOPE,
        &second_day,
        7_001,
        page(closed_events, "stack-drift-closed", CoverageStateV1::Known),
    )
    .unwrap();

    let model = project_execution_topology_fragments(SCOPE, &requested, 7_002, &[open, closed]);
    let closed_dimensions = [
        ExecutionTopologyDimensionV1::StackDriftKind(
            tracedecay_application::ExecutionStackDriftKindV1::HeadAdvanced,
        ),
        ExecutionTopologyDimensionV1::IntervalState(
            tracedecay_application::ExecutionIntervalStateV1::Closed,
        ),
        ExecutionTopologyDimensionV1::DurationBucket(
            tracedecay_application::ExecutionDurationBucketV1::From1dTo7d,
        ),
    ];
    assert_eq!(
        find(&model, "work_stale_stack_age_seconds", &closed_dimensions)
            .value
            .value,
        Some(5.0)
    );
    assert!(model.measurements.iter().all(|measurement| {
        !matches!(
            measurement.dimensions.as_slice(),
            [
                ExecutionTopologyDimensionV1::StackDriftKind(_),
                ExecutionTopologyDimensionV1::IntervalState(
                    tracedecay_application::ExecutionIntervalStateV1::Open
                ),
                ExecutionTopologyDimensionV1::DurationBucket(_),
            ]
        )
    }));
}

#[test]
fn canonical_closed_drift_with_a_false_age_bucket_fails_closed() {
    let day = horizon(0, DAY_MICROS);
    let first_observed = 60_000_000;
    let terminal = 120_000_000;
    let fragment = build_execution_topology_rollup_fragment(
        SCOPE,
        &day,
        8_000,
        page(
            vec![envelope(
                8_001,
                terminal,
                "trace.stack-drift.tamper",
                ObservabilityPayloadV1::WorkStackDrift(WorkStackDriftObservedV1 {
                    kind: StackDriftKindV1::HeadAdvanced,
                    state: IntervalStateV1::Closed,
                    first_observed_micros: first_observed,
                    terminal_micros: Some(terminal),
                    age_bucket: DurationBucketV1::From1mTo5m,
                    coverage: CoverageStateV1::Known,
                }),
                (None, None),
                CoverageStateV1::Known,
                (0, "boot.stack-drift"),
            )],
            "stack-drift-tamper",
            CoverageStateV1::Known,
        ),
    )
    .unwrap();
    let mut canonical = serde_json::to_value(fragment).unwrap();
    let rows = canonical
        .pointer_mut("/state/reduced/lifecycle_carry/stack_drifts")
        .and_then(serde_json::Value::as_object_mut)
        .expect("canonical drift carry map");
    let row = rows.values_mut().next().expect("one retained drift row");
    row["age_bucket"] = serde_json::json!("under1m");
    row["content_digest"] = serde_json::json!(
        tracedecay_domain::canonical_sha256(&(
            StackDriftKindV1::HeadAdvanced,
            IntervalStateV1::Closed,
            first_observed,
            Some(terminal),
            DurationBucketV1::Under1m,
            CoverageStateV1::Known,
        ))
        .unwrap()
        .as_str()
    );
    let tampered = serde_json::from_value(canonical).unwrap();

    assert_store_unavailable(&project_execution_topology_fragments(
        SCOPE,
        &day,
        8_002,
        &[tampered],
    ));
}
