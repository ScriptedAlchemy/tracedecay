use tracedecay_application::{
    ObservabilityHorizonV1, ObservabilityPageV1, build_execution_topology_daily_rollup,
    project_execution_topology_fragments,
};
use tracedecay_domain::{
    CoverageStateV1, ObservabilityEnvelopeV1, ObservabilityPayloadV1,
    ObservabilityRetentionClassV1, ObservabilityTerminalResultV1, TelemetryDropObservedV1,
};

const DAY_MICROS: i64 = 86_400_000_000;
const SCOPE: &str = "project.execution-topology-terminal";

fn terminal(index: u64, clean: bool) -> ObservabilityEnvelopeV1 {
    let boot = format!("boot.execution-topology-terminal.{index}");
    let payload = ObservabilityPayloadV1::TelemetryDrop(TelemetryDropObservedV1 {
        first_missing_sequence: 1,
        last_missing_sequence: 1,
        proved_drop_lower_bound: 0,
        clean_shutdown_observed: clean,
    });
    let envelope = ObservabilityEnvelopeV1 {
        event_id: format!("event.execution-topology-terminal.{index}"),
        event_kind: payload.event_kind().to_owned(),
        schema_revision: 1,
        idempotency_key: format!("idempotency.execution-topology-terminal.{index}"),
        trace_id: boot.clone(),
        scope_ref: SCOPE.to_owned(),
        capability: "observability".to_owned(),
        operation: "drop".to_owned(),
        event_time_micros: i64::try_from(index).expect("small fixture index") + 1,
        observation_time_micros: i64::try_from(index).expect("small fixture index") + 1,
        valid_from_micros: None,
        valid_until_micros: None,
        quantity: Some(0.0),
        unit: Some("events".to_owned()),
        terminal_result: Some(if clean {
            ObservabilityTerminalResultV1::Succeeded
        } else {
            ObservabilityTerminalResultV1::Unknown
        }),
        producer_revision: "producer.v1".to_owned(),
        configuration_revision: "configuration.v1".to_owned(),
        policy_revision: "policy.v1".to_owned(),
        watermark: format!("{boot}:1"),
        coverage: if clean {
            CoverageStateV1::Known
        } else {
            CoverageStateV1::Unknown
        },
        sampling_probability: None,
        retention_class: ObservabilityRetentionClassV1::LocalRollup395d,
        emitted_count: 1,
        delayed_count: 0,
        dropped_count: 0,
        process_boot_id: boot,
        producer_sequence: 1,
        payload,
    };
    envelope.validate().expect("valid terminal fixture");
    envelope
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

#[test]
fn clean_zero_drop_terminals_close_without_consuming_drop_carry() {
    let horizon = ObservabilityHorizonV1 {
        since_micros: 0,
        until_micros: DAY_MICROS,
    };
    let build = build_execution_topology_daily_rollup(
        SCOPE,
        &horizon,
        DAY_MICROS,
        page(
            (0..513).map(|index| terminal(index, true)).collect(),
            "terminals:513",
        ),
    )
    .expect("zero-drop terminals fit the ordinary reduced state");
    assert_eq!(build.coverage, CoverageStateV1::Known);

    let model =
        project_execution_topology_fragments(SCOPE, &horizon, DAY_MICROS, &[build.fragment]);
    assert!(model.current);
    assert_eq!(model.coverage.state, CoverageStateV1::Known);
    assert_eq!(model.emission_coverage.dropped, Some(0));
}

#[test]
fn nonclean_zero_drop_terminal_keeps_coverage_unknown() {
    let horizon = ObservabilityHorizonV1 {
        since_micros: 0,
        until_micros: DAY_MICROS,
    };
    let build = build_execution_topology_daily_rollup(
        SCOPE,
        &horizon,
        DAY_MICROS,
        page(vec![terminal(1, false)], "terminal:unclean"),
    )
    .expect("unclean terminal remains typed retained evidence");
    let model =
        project_execution_topology_fragments(SCOPE, &horizon, DAY_MICROS, &[build.fragment]);

    assert!(!model.current);
    assert_eq!(model.coverage.state, CoverageStateV1::Unknown);
    assert_eq!(model.emission_coverage.dropped, Some(0));
}

#[test]
fn carried_positive_drop_then_clean_terminal_remains_partial() {
    let horizon = ObservabilityHorizonV1 {
        since_micros: 0,
        until_micros: DAY_MICROS,
    };
    let mut carried = terminal(1, false);
    let ObservabilityPayloadV1::TelemetryDrop(carried_drop) = &mut carried.payload else {
        unreachable!()
    };
    carried_drop.proved_drop_lower_bound = 1;
    carried.quantity = Some(1.0);
    carried.terminal_result = Some(ObservabilityTerminalResultV1::Partial);
    carried.coverage = CoverageStateV1::Partial;
    carried.dropped_count = 1;
    carried.validate().expect("valid carried positive receipt");

    let mut clean = terminal(1, true);
    clean.event_id = "event.execution-topology-terminal.clean".to_owned();
    clean.idempotency_key = "idempotency.execution-topology-terminal.clean".to_owned();
    clean.event_time_micros = 2;
    clean.observation_time_micros = 2;
    clean.producer_sequence = 2;
    clean.watermark = format!("{}:2", clean.process_boot_id);
    let ObservabilityPayloadV1::TelemetryDrop(clean_drop) = &mut clean.payload else {
        unreachable!()
    };
    clean_drop.first_missing_sequence = 2;
    clean_drop.last_missing_sequence = 2;
    clean.validate().expect("valid clean terminal");

    let build = build_execution_topology_daily_rollup(
        SCOPE,
        &horizon,
        DAY_MICROS,
        page(vec![carried, clean], "terminal:carried-and-clean"),
    )
    .expect("carried loss remains retained partial evidence");
    let model =
        project_execution_topology_fragments(SCOPE, &horizon, DAY_MICROS, &[build.fragment]);

    assert!(!model.current);
    assert_eq!(model.coverage.state, CoverageStateV1::Partial);
    assert_eq!(model.emission_coverage.dropped, Some(1));
}
