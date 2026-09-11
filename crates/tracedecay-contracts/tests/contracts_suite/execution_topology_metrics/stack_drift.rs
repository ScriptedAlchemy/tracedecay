use super::*;
use tracedecay_domain::{
    DurationBucketV1, IntervalStateV1, StackDriftKindV1, WorkStackDriftObservedV1,
};

#[tokio::test]
async fn observed_open_stack_drift_publishes_its_bounded_age_cell() {
    let events = (0..5_u64)
        .map(|index| {
            envelope(
                100 + index,
                &format!("trace.stack-drift.{index}"),
                ObservabilityPayloadV1::WorkStackDrift(WorkStackDriftObservedV1 {
                    kind: StackDriftKindV1::BaseAdvanced,
                    state: IntervalStateV1::Open,
                    first_observed_micros: 0,
                    terminal_micros: None,
                    age_bucket: DurationBucketV1::Under1m,
                    coverage: CoverageStateV1::Known,
                }),
                None,
            )
        })
        .collect();

    let model = read(&Observations::Page(page(events))).await;
    let expected_dimensions = serde_json::json!([
        { "dimension": "stack_drift_kind", "value": "base_advanced" },
        { "dimension": "interval_state", "value": "open" },
        { "dimension": "duration_bucket", "value": "under1m" }
    ]);
    let cell = model
        .measurements
        .iter()
        .find(|measurement| {
            measurement.value.metric == "work_stale_stack_age_seconds"
                && serde_json::to_value(&measurement.dimensions).ok()
                    == Some(expected_dimensions.clone())
        })
        .expect("the bounded open drift cell is projected");

    assert_eq!(cell.value.value, Some(5.0));
    assert_eq!(cell.value.denominator, "observed_stack_drifts");
    assert_eq!(cell.value.coverage.eligible, Some(5));
    assert_eq!(cell.value.coverage.observed, 5);
    assert_eq!(cell.unavailable, None);
}

#[tokio::test]
async fn delayed_open_observation_cannot_reopen_a_closed_drift_interval() {
    let mut events = Vec::new();
    for index in 0..5_u64 {
        let trace = format!("trace.stack-drift.closed.{index}");
        let mut closed = envelope(
            200 + index,
            &trace,
            ObservabilityPayloadV1::WorkStackDrift(WorkStackDriftObservedV1 {
                kind: StackDriftKindV1::BaseAdvanced,
                state: IntervalStateV1::Closed,
                first_observed_micros: 0,
                terminal_micros: Some(2_000),
                age_bucket: DurationBucketV1::Under1m,
                coverage: CoverageStateV1::Known,
            }),
            None,
        );
        closed.event_time_micros = 2_000;
        closed.observation_time_micros = 2_001;
        let mut delayed_open = envelope(
            300 + index,
            &trace,
            ObservabilityPayloadV1::WorkStackDrift(WorkStackDriftObservedV1 {
                kind: StackDriftKindV1::BaseAdvanced,
                state: IntervalStateV1::Open,
                first_observed_micros: 0,
                terminal_micros: None,
                age_bucket: DurationBucketV1::Under1m,
                coverage: CoverageStateV1::Known,
            }),
            None,
        );
        delayed_open.event_time_micros = 3_000;
        delayed_open.observation_time_micros = 3_001;
        events.extend([closed, delayed_open]);
    }

    let model = read(&Observations::Page(page(events))).await;
    let expected_dimensions = serde_json::json!([
        { "dimension": "stack_drift_kind", "value": "base_advanced" },
        { "dimension": "interval_state", "value": "closed" },
        { "dimension": "duration_bucket", "value": "under1m" }
    ]);
    let closed = model
        .measurements
        .iter()
        .find(|measurement| {
            measurement.value.metric == "work_stale_stack_age_seconds"
                && serde_json::to_value(&measurement.dimensions).ok()
                    == Some(expected_dimensions.clone())
        })
        .expect("the terminal drift state remains closed");

    assert_eq!(closed.value.value, Some(5.0));
}
