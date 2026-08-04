use super::*;

#[test]
fn activity_detail_is_a_finite_safe_vocabulary() {
    let mut envelope = ObservabilityEnvelopeV1 {
        event_id: "event:activity:1".into(),
        event_kind: "activity.observed.v1".into(),
        schema_revision: 1,
        idempotency_key: "idempotency:activity:1".into(),
        trace_id: "trace:activity:1".into(),
        scope_ref: "scope:activity".into(),
        capability: "activity".into(),
        operation: "hook".into(),
        event_time_micros: 1,
        observation_time_micros: 1,
        valid_from_micros: Some(1),
        valid_until_micros: None,
        quantity: Some(1.0),
        unit: Some("events".into()),
        terminal_result: Some(ObservabilityTerminalResultV1::Succeeded),
        producer_revision: "activity-observer.v1".into(),
        configuration_revision: "registered-project-session.v1".into(),
        policy_revision: "local-activity-retention.v1".into(),
        watermark: "activity:1".into(),
        coverage: CoverageStateV1::Known,
        sampling_probability: None,
        retention_class: ObservabilityRetentionClassV1::OptionalLocalDetail30d,
        emitted_count: 1,
        delayed_count: 0,
        dropped_count: 0,
        process_boot_id: "boot:activity".into(),
        producer_sequence: 1,
        payload: ObservabilityPayloadV1::Activity(ActivityObservedV1 {
            family: "hook".into(),
            units: 1,
            detail: Some("session_boundary".into()),
        }),
    };
    assert_eq!(envelope.validate(), Ok(()));

    let ObservabilityPayloadV1::Activity(activity) = &mut envelope.payload else {
        unreachable!();
    };
    activity.detail = Some("external-hook-name".into());
    assert_eq!(envelope.validate(), Err("activity"));
    assert_eq!(
        ActivityObservedV1::bounded_detail("hook", Some("external-hook-name")),
        None
    );
}
