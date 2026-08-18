use tracedecay_application::{ObservabilityHorizonV1, ObservabilityRecordPort, now_micros};
use tracedecay_domain::{
    AdoptionEligibilityObservedV1, AdoptionOutcomeLinkedV1, AnalyticsConsentChangedV1,
    AnalyticsModeV1, ContextOutcomeObservedV1, CoverageStateV1, LatencyObservedV1, LatencyStageV1,
    ObservabilityEnvelopeV1, ObservabilityPayloadV1, ObservabilityRetentionClassV1,
    ObservabilityTerminalResultV1, OperationAvailabilityV1, OperationResourceObservedV1,
    RejectedArgumentErrorClassV1, RejectedArgumentNameV1, RejectedArgumentObservedV1,
    RejectedArgumentSurfaceV1, RetrievalAblationObservedV1, RetrieverObservedV1,
};

use super::{RegisteredObservabilityPortV1, observatory_read_model};

fn envelope(sequence: u64, payload: ObservabilityPayloadV1) -> ObservabilityEnvelopeV1 {
    let observed = now_micros().0.saturating_sub(1_000_000);
    envelope_at(sequence, observed.saturating_add(sequence as i64), payload)
}

fn envelope_at(
    sequence: u64,
    observed_at_micros: i64,
    payload: ObservabilityPayloadV1,
) -> ObservabilityEnvelopeV1 {
    ObservabilityEnvelopeV1 {
        event_id: format!("event:{sequence}"),
        event_kind: payload.event_kind().to_owned(),
        schema_revision: 1,
        idempotency_key: format!("idempotency:{sequence}"),
        trace_id: format!("trace:{sequence}"),
        scope_ref: "scope:observatory-projection".to_owned(),
        capability: "observatory".to_owned(),
        operation: "project".to_owned(),
        event_time_micros: observed_at_micros,
        observation_time_micros: observed_at_micros,
        valid_from_micros: Some(observed_at_micros),
        valid_until_micros: None,
        quantity: None,
        unit: None,
        terminal_result: Some(ObservabilityTerminalResultV1::Succeeded),
        producer_revision: "observatory-test-producer.v1".to_owned(),
        configuration_revision: "observatory-test-configuration.v1".to_owned(),
        policy_revision: "observatory-test-policy.v1".to_owned(),
        watermark: format!("producer:{sequence}"),
        coverage: CoverageStateV1::Known,
        sampling_probability: None,
        retention_class: ObservabilityRetentionClassV1::LocalRollup395d,
        emitted_count: 1,
        delayed_count: 0,
        dropped_count: 0,
        process_boot_id: "boot:observatory-projection".to_owned(),
        producer_sequence: sequence,
        payload,
    }
}

fn metric_value(model: &tracedecay_application::ObservatoryReadModelV1, name: &str) -> f64 {
    model
        .metrics
        .iter()
        .find(|metric| metric.metric == name)
        .unwrap_or_else(|| panic!("missing metric {name}"))
        .value
        .unwrap_or_else(|| panic!("metric {name} was not measured"))
}

#[tokio::test]
async fn canonical_read_projects_recorded_adoption_retrieval_and_performance_families() {
    let harness = tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness::open(
        "observatory-plan26-projection",
    )
    .await;
    let port = RegisteredObservabilityPortV1::new(&harness.registered);
    let payloads = [
        ObservabilityPayloadV1::AdoptionEligibility(AdoptionEligibilityObservedV1 {
            capability: "retrieval".to_owned(),
            eligible: 100,
            enabled: 80,
            available: 70,
        }),
        ObservabilityPayloadV1::AdoptionOutcome(AdoptionOutcomeLinkedV1 {
            invoked: 60,
            terminal: 50,
            independently_useful: 30,
            repeat_useful: 20,
            censored: 5,
            unknown: 5,
        }),
        ObservabilityPayloadV1::Retriever(RetrieverObservedV1 {
            retriever_kind: "lexical".to_owned(),
            profile_revision: "retriever-profile.v1".to_owned(),
            requested_candidates: 40,
            consumed_candidates: 30,
            eligible_candidates: 25,
            returned_candidates: 10,
            unique_contributions: 4,
        }),
        ObservabilityPayloadV1::ContextOutcome(ContextOutcomeObservedV1 {
            outcome: "independently_verified_use".to_owned(),
            independently_observed: true,
            censored: false,
        }),
        ObservabilityPayloadV1::RetrievalAblation(RetrievalAblationObservedV1 {
            descriptor_revision: "equal-budget-ablation.v1".to_owned(),
            baseline_value: 0.6,
            candidate_value: 0.75,
            unit: "ratio".to_owned(),
            coverage: CoverageStateV1::Known,
        }),
        ObservabilityPayloadV1::OperationResource(Box::new(OperationResourceObservedV1 {
            provider_request_id: None,
            scheduled_latency_micros: 10,
            service_latency_micros: 100,
            process_rss_bytes: Some(4_096),
            process_pss_bytes: Some(3_000),
            cpu_user_micros: Some(40),
            cpu_system_micros: Some(10),
            read_bytes: Some(200),
            write_bytes: Some(100),
            input_tokens: None,
            output_tokens: None,
            cost_amount: None,
            cost_currency: None,
            pricing_revision: None,
            stage_timings: Vec::new(),
            phase_timings: Vec::new(),
            absolute_deadline_micros: None,
            availability: OperationAvailabilityV1::Available,
            activation_outcome: None,
            process_count: Some(1),
            input_bytes: Some(100),
            output_bytes: Some(50),
        })),
        ObservabilityPayloadV1::OperationResource(Box::new(OperationResourceObservedV1 {
            provider_request_id: None,
            scheduled_latency_micros: 20,
            service_latency_micros: 200,
            process_rss_bytes: Some(8_192),
            process_pss_bytes: Some(6_000),
            cpu_user_micros: Some(50),
            cpu_system_micros: Some(20),
            read_bytes: Some(300),
            write_bytes: Some(150),
            input_tokens: None,
            output_tokens: None,
            cost_amount: None,
            cost_currency: None,
            pricing_revision: None,
            stage_timings: Vec::new(),
            phase_timings: Vec::new(),
            absolute_deadline_micros: None,
            availability: OperationAvailabilityV1::Available,
            activation_outcome: None,
            process_count: Some(1),
            input_bytes: Some(200),
            output_bytes: Some(100),
        })),
        ObservabilityPayloadV1::Latency(LatencyObservedV1 {
            stage: LatencyStageV1::Queue,
            scheduled_arrival_micros: 7,
            service_micros: 13,
            deadline_budget_micros: Some(100),
            coverage: CoverageStateV1::Known,
        }),
    ];
    for (index, payload) in payloads.into_iter().enumerate() {
        port.record(envelope((index + 1) as u64, payload))
            .await
            .expect("record canonical observation");
    }

    let now_seconds = now_micros().0.div_euclid(1_000_000);
    let model = observatory_read_model(
        &harness.registered,
        Some("scope:observatory-projection"),
        now_seconds.saturating_sub(60),
    )
    .await;

    assert_eq!(metric_value(&model, "adoption_eligible"), 100.0);
    assert_eq!(metric_value(&model, "adoption_repeat_useful"), 20.0);
    assert_eq!(metric_value(&model, "retriever_consumed_candidates"), 30.0);
    assert_eq!(metric_value(&model, "retriever_unique_contributions"), 4.0);
    assert_eq!(metric_value(&model, "retrieval_task_outcome_linkage"), 1.0);
    let ablation = metric_value(&model, "retrieval_equal_budget_ablation");
    assert!(
        (ablation - 0.15).abs() < 1e-12,
        "equal-budget ablation was {ablation}"
    );
    assert_eq!(metric_value(&model, "operation_latency_p50"), 100.0);
    assert_eq!(metric_value(&model, "operation_latency_p95"), 200.0);
    assert_eq!(metric_value(&model, "queue_span_p95"), 13.0);
    assert_eq!(metric_value(&model, "process_rss_peak"), 8_192.0);
    assert_eq!(metric_value(&model, "cpu_time_total"), 120.0);
    assert_eq!(metric_value(&model, "io_amplification"), 5.0 / 3.0);
    assert_eq!(metric_value(&model, "observability_eligible_events"), 8.0);
    assert_eq!(metric_value(&model, "observability_late_arrivals"), 0.0);
}

#[tokio::test]
async fn canonical_read_selects_latest_consent_and_keeps_unrecorded_evidence_unknown() {
    let harness = tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness::open(
        "observatory-controls-projection",
    )
    .await;
    let port = RegisteredObservabilityPortV1::new(&harness.registered);
    let consent_observed_at = now_micros().0.saturating_sub(1_000_000);
    port.record(envelope_at(
        2,
        consent_observed_at,
        ObservabilityPayloadV1::AnalyticsConsent(AnalyticsConsentChangedV1 {
            previous: AnalyticsModeV1::AggregateShare,
            current: AnalyticsModeV1::LocalOnly,
            share_staging_age_seconds: Some(17),
        }),
    ))
    .await
    .expect("record latest consent");
    port.record(envelope_at(
        1,
        consent_observed_at,
        ObservabilityPayloadV1::AnalyticsConsent(AnalyticsConsentChangedV1 {
            previous: AnalyticsModeV1::LocalOnly,
            current: AnalyticsModeV1::AggregateShare,
            share_staging_age_seconds: None,
        }),
    ))
    .await
    .expect("record older consent after latest row");

    let now_seconds = now_micros().0.div_euclid(1_000_000);
    let model = observatory_read_model(
        &harness.registered,
        Some("scope:observatory-projection"),
        now_seconds.saturating_sub(60),
    )
    .await;

    assert_eq!(
        model.analytics_mode.current,
        Some(AnalyticsModeV1::LocalOnly)
    );
    assert_eq!(
        metric_value(&model, "analytics_share_staging_age_seconds"),
        17.0
    );
    let egress = model
        .metrics
        .iter()
        .find(|metric| metric.metric == "analytics_egress_failures")
        .expect("egress projection");
    assert_eq!(egress.value, None);
    assert_eq!(egress.coverage.state, CoverageStateV1::Unknown);
    assert_eq!(
        egress.unavailable_reason.as_deref(),
        Some("egress_attempts_not_recorded")
    );
    assert_eq!(model.comparison.baseline_build, None);
    assert_eq!(
        model.comparison.disposition,
        tracedecay_application::ComparisonDispositionV1::InsufficientEvidence
    );
    assert_eq!(model.comparison.coverage.state, CoverageStateV1::Unknown);
    assert!(model.current, "the retained source frontier is current");
    assert_eq!(
        model.horizon,
        ObservabilityHorizonV1 {
            since_micros: now_seconds.saturating_sub(60).saturating_mul(1_000_000),
            until_micros: model.observed_at_micros,
        }
    );
}

#[tokio::test]
async fn rejected_argument_read_model_counts_seeded_observations_exactly() {
    let harness = tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness::open(
        "observatory-rejected-argument-projection",
    )
    .await;
    let port = RegisteredObservabilityPortV1::new(&harness.registered);
    let payloads = [
        RejectedArgumentObservedV1 {
            surface: RejectedArgumentSurfaceV1::Cli,
            operation: "feedback_diagnostics".to_owned(),
            argument: RejectedArgumentNameV1::RequestBody,
            error_class: RejectedArgumentErrorClassV1::InvalidShape,
            schema_revision: 1,
        },
        RejectedArgumentObservedV1 {
            surface: RejectedArgumentSurfaceV1::Cli,
            operation: "feedback_diagnostics".to_owned(),
            argument: RejectedArgumentNameV1::RequestBody,
            error_class: RejectedArgumentErrorClassV1::InvalidShape,
            schema_revision: 1,
        },
        RejectedArgumentObservedV1 {
            surface: RejectedArgumentSurfaceV1::Mcp,
            operation: "feedback_list".to_owned(),
            argument: RejectedArgumentNameV1::Operation,
            error_class: RejectedArgumentErrorClassV1::Unauthorized,
            schema_revision: 1,
        },
        RejectedArgumentObservedV1 {
            surface: RejectedArgumentSurfaceV1::Http,
            operation: "feedback_get".to_owned(),
            argument: RejectedArgumentNameV1::RequestHandle,
            error_class: RejectedArgumentErrorClassV1::InvalidShape,
            schema_revision: 1,
        },
    ];
    for (index, value) in payloads.into_iter().enumerate() {
        port.record(envelope(
            (index + 1) as u64,
            ObservabilityPayloadV1::RejectedArgument(value),
        ))
        .await
        .expect("record rejected-argument observation");
    }

    let now_seconds = now_micros().0.div_euclid(1_000_000);
    let model = observatory_read_model(
        &harness.registered,
        Some("scope:observatory-projection"),
        now_seconds.saturating_sub(60),
    )
    .await;

    let rejected = &model.rejected_arguments;
    assert_eq!(rejected.coverage.state, CoverageStateV1::Known);
    assert_eq!(rejected.rejected_total, Some(4));
    assert_eq!(rejected.eligible_attempts, None);
    assert_eq!(
        rejected.rejection_rate, None,
        "rate stays unavailable when the attempt denominator is unknown"
    );
    assert_eq!(rejected.redacted_name_count, 0);
    assert_eq!(rejected.groups.len(), 3);
    assert_eq!(
        rejected
            .groups
            .iter()
            .find(|group| {
                group.surface == RejectedArgumentSurfaceV1::Cli
                    && group.operation == "feedback_diagnostics"
                    && group.argument == RejectedArgumentNameV1::RequestBody
                    && group.error_class == RejectedArgumentErrorClassV1::InvalidShape
            })
            .map(|group| group.count),
        Some(2)
    );
    assert_eq!(
        rejected
            .groups
            .iter()
            .find(|group| group.surface == RejectedArgumentSurfaceV1::Mcp)
            .map(|group| group.count),
        Some(1)
    );
    assert_eq!(
        rejected
            .groups
            .iter()
            .find(|group| group.surface == RejectedArgumentSurfaceV1::Http)
            .map(|group| group.count),
        Some(1)
    );
    assert!(
        rejected.groups.iter().all(|group| group.rate.is_none()),
        "per-cell rates stay absent without an eligible-attempt denominator"
    );
}

#[tokio::test]
async fn rejected_argument_read_model_does_not_fabricate_empty_zero_without_family() {
    let harness = tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness::open(
        "observatory-rejected-argument-empty",
    )
    .await;
    let now_seconds = now_micros().0.div_euclid(1_000_000);
    let model = observatory_read_model(
        &harness.registered,
        Some("scope:observatory-projection"),
        now_seconds.saturating_sub(60),
    )
    .await;
    let rejected = &model.rejected_arguments;
    assert_eq!(rejected.rejected_total, None);
    assert_eq!(rejected.rejection_rate, None);
    assert!(rejected.groups.is_empty());
    assert_eq!(rejected.coverage.state, CoverageStateV1::Unknown);
    assert_eq!(
        rejected.unavailable_reason.as_deref(),
        Some("rejected_argument_observations_not_recorded")
    );
}
