use std::collections::BTreeMap;

use tracedecay_domain::{
    CanonicalObservationIdV1, ObservationId, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceRangeV1, ProjectId, ProviderId, ProviderUsageCounterSemanticsV1,
    ProviderUsageCountersV1, ProviderUsageCursorV1, ProviderUsageModelV1,
    ProviderUsageObservationV1, ProviderUsageReadV1, ProviderUsageScopeV1, SessionId,
};

use super::{
    AggregatedProviderUsageCountersV1, ProviderUsageCoverageV1, ProviderUsageIssueKindV1,
    ProviderUsageScanV1, ScanStep, price_provider_usage, reduce_provider_usage,
};
use crate::provider_pricing::{ModelPrice, PriceTable};

fn id(seed: u8) -> CanonicalObservationIdV1 {
    CanonicalObservationIdV1::new(format!("sha256:{seed:064x}")).expect("valid digest")
}

fn observation(
    sequence: u64,
    ordinal: u32,
    provider: &str,
    session: &str,
    semantics: ProviderUsageCounterSemanticsV1,
    counters: ProviderUsageCountersV1,
) -> ProviderUsageObservationV1 {
    ProviderUsageObservationV1 {
        observation_id: id(sequence as u8),
        usage_ordinal: ordinal,
        receipt_id: format!("receipt:{sequence}"),
        observation_sequence: sequence,
        scope: ObservationScopeV1::Profile,
        provider: ProviderId::new(provider).expect("provider"),
        model: ProviderUsageModelV1::Known {
            model: if provider == "codex" {
                "openai/gpt-5.6-codex".to_owned()
            } else {
                "anthropic/claude-sonnet-4.6".to_owned()
            },
        },
        native_scope: if provider == "codex" {
            ProviderUsageScopeV1::Request
        } else {
            ProviderUsageScopeV1::Message
        },
        counter_semantics: semantics,
        counters,
        session_id: SessionId::new(session).expect("session"),
        turn_id: Some(ObservationId::new(format!("turn:{sequence}")).expect("turn")),
        message_id: Some(ObservationId::new(format!("message:{sequence}")).expect("message")),
        request_id: Some(ObservationId::new(format!("request:{sequence}")).expect("request")),
        native_kind: "token_count".to_owned(),
        native_field: "fixture.usage".to_owned(),
        ordering_domain: ObservationOrderingDomainV1::FileBytes,
        source_range: ObservationSourceRangeV1::new(sequence * 10, sequence * 10 + 5)
            .expect("range"),
        native_timestamp: Some(1_700_000_000 + sequence as i64),
    }
}

fn counters(input: u64, output: u64) -> ProviderUsageCountersV1 {
    ProviderUsageCountersV1::Known {
        input_tokens: Some(input),
        output_tokens: Some(output),
        cache_read_tokens: None,
        cache_write_tokens: None,
        reasoning_tokens: None,
        total_tokens: Some(input + output),
    }
}

fn totals(input: u64, output: u64) -> AggregatedProviderUsageCountersV1 {
    AggregatedProviderUsageCountersV1 {
        input_tokens: Some(input),
        output_tokens: Some(output),
        cache_read_tokens: None,
        cache_write_tokens: None,
        reasoning_tokens: None,
        total_tokens: Some(input + output),
    }
}

#[test]
fn paired_codex_delta_and_checkpoint_count_once() {
    let aggregate = reduce_provider_usage(&[
        observation(
            1,
            0,
            "codex",
            "session-a",
            ProviderUsageCounterSemanticsV1::Delta,
            counters(10, 2),
        ),
        observation(
            1,
            1,
            "codex",
            "session-a",
            ProviderUsageCounterSemanticsV1::Cumulative,
            counters(100, 20),
        ),
    ]);

    assert_eq!(aggregate.coverage, ProviderUsageCoverageV1::Complete);
    assert_eq!(aggregate.totals, totals(10, 2));
    assert_eq!(aggregate.deltas.len(), 1);
    assert_eq!(aggregate.deltas[0].scope, ObservationScopeV1::Profile);
    assert_eq!(aggregate.deltas[0].provider, "codex");
    assert_eq!(aggregate.deltas[0].session_id, "session-a");
    assert_eq!(aggregate.deltas[0].turn_id.as_deref(), Some("turn:1"));
    assert_eq!(aggregate.deltas[0].message_id.as_deref(), Some("message:1"));
    assert_eq!(aggregate.deltas[0].request_id.as_deref(), Some("request:1"));
}

#[test]
fn paired_unknown_model_emits_one_model_issue() {
    let mut native = observation(
        1,
        0,
        "codex",
        "session-a",
        ProviderUsageCounterSemanticsV1::Delta,
        counters(10, 2),
    );
    native.model = ProviderUsageModelV1::Unknown {
        reason: tracedecay_domain::CanonicalUnknownStateV1::Absent,
    };
    let mut checkpoint = observation(
        1,
        1,
        "codex",
        "session-a",
        ProviderUsageCounterSemanticsV1::Cumulative,
        counters(100, 20),
    );
    checkpoint.model = ProviderUsageModelV1::Unknown {
        reason: tracedecay_domain::CanonicalUnknownStateV1::Absent,
    };

    let aggregate = reduce_provider_usage(&[native, checkpoint]);

    assert_eq!(aggregate.deltas.len(), 1);
    assert_eq!(
        aggregate
            .issues
            .iter()
            .filter(|issue| issue.kind == ProviderUsageIssueKindV1::UnknownModel)
            .count(),
        1
    );
}

#[test]
fn paired_delta_is_rejected_when_its_checkpoint_does_not_advance() {
    let aggregate = reduce_provider_usage(&[
        observation(
            1,
            0,
            "codex",
            "session-a",
            ProviderUsageCounterSemanticsV1::Delta,
            counters(10, 2),
        ),
        observation(
            1,
            1,
            "codex",
            "session-a",
            ProviderUsageCounterSemanticsV1::Cumulative,
            counters(100, 20),
        ),
        observation(
            2,
            0,
            "codex",
            "session-a",
            ProviderUsageCounterSemanticsV1::Delta,
            counters(10, 2),
        ),
        observation(
            2,
            1,
            "codex",
            "session-a",
            ProviderUsageCounterSemanticsV1::Cumulative,
            counters(100, 20),
        ),
    ]);

    assert_eq!(aggregate.coverage, ProviderUsageCoverageV1::Partial);
    assert_eq!(aggregate.totals, totals(10, 2));
    assert_eq!(aggregate.deltas.len(), 1);
    assert!(aggregate.issues.iter().any(|issue| {
        issue.kind == ProviderUsageIssueKindV1::DuplicateCumulativeCheckpoint
            && issue.observation_sequence == Some(2)
    }));
}

#[test]
fn paired_delta_is_accepted_once_when_its_checkpoint_advances() {
    let aggregate = reduce_provider_usage(&[
        observation(
            1,
            0,
            "codex",
            "session-a",
            ProviderUsageCounterSemanticsV1::Delta,
            counters(10, 2),
        ),
        observation(
            1,
            1,
            "codex",
            "session-a",
            ProviderUsageCounterSemanticsV1::Cumulative,
            counters(100, 20),
        ),
        observation(
            2,
            0,
            "codex",
            "session-a",
            ProviderUsageCounterSemanticsV1::Delta,
            counters(5, 3),
        ),
        observation(
            2,
            1,
            "codex",
            "session-a",
            ProviderUsageCounterSemanticsV1::Cumulative,
            counters(105, 23),
        ),
    ]);

    assert_eq!(aggregate.coverage, ProviderUsageCoverageV1::Complete);
    assert_eq!(aggregate.totals, totals(15, 5));
    assert_eq!(aggregate.deltas.len(), 2);
}

#[test]
fn cumulative_only_progression_derives_only_the_monotonic_difference() {
    let aggregate = reduce_provider_usage(&[
        observation(
            1,
            0,
            "codex",
            "session-a",
            ProviderUsageCounterSemanticsV1::Cumulative,
            counters(100, 20),
        ),
        observation(
            2,
            0,
            "codex",
            "session-a",
            ProviderUsageCounterSemanticsV1::Cumulative,
            counters(135, 29),
        ),
    ]);

    assert_eq!(aggregate.coverage, ProviderUsageCoverageV1::Partial);
    assert_eq!(aggregate.totals, totals(35, 9));
    assert_eq!(aggregate.deltas.len(), 1);
    assert_eq!(aggregate.deltas[0].derived_from_sequence, Some(1));
    assert!(
        aggregate
            .issues
            .iter()
            .any(|issue| { issue.kind == ProviderUsageIssueKindV1::InitialCumulativeCheckpoint })
    );
}

#[test]
fn cumulative_model_transition_starts_a_new_domain_instead_of_cross_differencing() {
    let first = observation(
        1,
        0,
        "codex",
        "session-a",
        ProviderUsageCounterSemanticsV1::Cumulative,
        counters(100, 20),
    );
    let mut second = observation(
        2,
        0,
        "codex",
        "session-a",
        ProviderUsageCounterSemanticsV1::Cumulative,
        counters(7, 2),
    );
    second.model = ProviderUsageModelV1::Known {
        model: "openai/gpt-6-codex".to_owned(),
    };

    let aggregate = reduce_provider_usage(&[first, second]);

    assert_eq!(aggregate.coverage, ProviderUsageCoverageV1::Partial);
    assert!(aggregate.deltas.is_empty());
    assert_eq!(
        aggregate
            .issues
            .iter()
            .filter(|issue| { issue.kind == ProviderUsageIssueKindV1::InitialCumulativeCheckpoint })
            .count(),
        2
    );
    assert!(
        aggregate
            .issues
            .iter()
            .all(|issue| issue.kind != ProviderUsageIssueKindV1::CumulativeReset)
    );
}

#[test]
fn same_session_cross_project_checkpoints_never_cross_difference() {
    let mut first = observation(
        1,
        0,
        "codex",
        "session-a",
        ProviderUsageCounterSemanticsV1::Cumulative,
        counters(100, 20),
    );
    first.scope = ObservationScopeV1::Project {
        project_id: ProjectId::new("project-a").expect("project"),
    };
    let mut second = observation(
        2,
        0,
        "codex",
        "session-a",
        ProviderUsageCounterSemanticsV1::Cumulative,
        counters(105, 22),
    );
    second.scope = ObservationScopeV1::Project {
        project_id: ProjectId::new("project-b").expect("project"),
    };

    let aggregate = reduce_provider_usage(&[first, second]);

    assert!(aggregate.deltas.is_empty());
    assert_eq!(
        aggregate
            .issues
            .iter()
            .filter(|issue| { issue.kind == ProviderUsageIssueKindV1::InitialCumulativeCheckpoint })
            .count(),
        2
    );
}

#[test]
fn unknown_model_checkpoints_never_share_a_cumulative_domain() {
    let mut first = observation(
        1,
        0,
        "codex",
        "session-a",
        ProviderUsageCounterSemanticsV1::Cumulative,
        counters(100, 20),
    );
    first.model = ProviderUsageModelV1::Unknown {
        reason: tracedecay_domain::CanonicalUnknownStateV1::Absent,
    };
    let mut second = observation(
        2,
        0,
        "codex",
        "session-a",
        ProviderUsageCounterSemanticsV1::Cumulative,
        counters(105, 22),
    );
    second.model = ProviderUsageModelV1::Unknown {
        reason: tracedecay_domain::CanonicalUnknownStateV1::Absent,
    };

    let aggregate = reduce_provider_usage(&[first, second]);

    assert!(aggregate.deltas.is_empty());
    assert_eq!(
        aggregate
            .issues
            .iter()
            .filter(|issue| { issue.kind == ProviderUsageIssueKindV1::InitialCumulativeCheckpoint })
            .count(),
        2
    );
}

#[test]
fn duplicate_checkpoint_is_rejected_instead_of_becoming_a_zero_delta() {
    let aggregate = reduce_provider_usage(&[
        observation(
            1,
            0,
            "codex",
            "session-a",
            ProviderUsageCounterSemanticsV1::Cumulative,
            counters(100, 20),
        ),
        observation(
            2,
            0,
            "codex",
            "session-a",
            ProviderUsageCounterSemanticsV1::Cumulative,
            counters(100, 20),
        ),
    ]);

    assert_eq!(aggregate.coverage, ProviderUsageCoverageV1::Partial);
    assert_eq!(
        aggregate.totals,
        AggregatedProviderUsageCountersV1::unknown()
    );
    assert!(aggregate.deltas.is_empty());
    assert!(
        aggregate
            .issues
            .iter()
            .any(|issue| { issue.kind == ProviderUsageIssueKindV1::DuplicateCumulativeCheckpoint })
    );
}

#[test]
fn cumulative_decrease_is_a_typed_reset_and_never_underflows() {
    let aggregate = reduce_provider_usage(&[
        observation(
            1,
            0,
            "codex",
            "session-a",
            ProviderUsageCounterSemanticsV1::Cumulative,
            counters(100, 20),
        ),
        observation(
            2,
            0,
            "codex",
            "session-a",
            ProviderUsageCounterSemanticsV1::Cumulative,
            counters(7, 2),
        ),
    ]);

    assert_eq!(aggregate.coverage, ProviderUsageCoverageV1::Partial);
    assert_eq!(
        aggregate.totals,
        AggregatedProviderUsageCountersV1::unknown()
    );
    assert!(aggregate.deltas.is_empty());
    assert!(
        aggregate
            .issues
            .iter()
            .any(|issue| issue.kind == ProviderUsageIssueKindV1::CumulativeReset)
    );
}

#[test]
fn mixed_provider_native_deltas_preserve_provenance_and_sum_exactly() {
    let aggregate = reduce_provider_usage(&[
        observation(
            1,
            0,
            "claude",
            "claude-session",
            ProviderUsageCounterSemanticsV1::Delta,
            counters(11, 3),
        ),
        observation(
            2,
            0,
            "codex",
            "codex-session",
            ProviderUsageCounterSemanticsV1::Delta,
            counters(5, 7),
        ),
    ]);

    assert_eq!(aggregate.coverage, ProviderUsageCoverageV1::Complete);
    assert_eq!(aggregate.totals, totals(16, 10));
    assert_eq!(
        aggregate
            .deltas
            .iter()
            .map(|delta| delta.provider.as_str())
            .collect::<Vec<_>>(),
        vec!["claude", "codex"]
    );
    assert_eq!(aggregate.deltas[0].native_timestamp, Some(1_700_000_001));
    assert_eq!(aggregate.deltas[0].native_field, "fixture.usage");
}

#[test]
fn malformed_or_unknown_counters_make_coverage_partial_without_zero_fill() {
    let aggregate = reduce_provider_usage(&[
        observation(
            1,
            0,
            "claude",
            "claude-session",
            ProviderUsageCounterSemanticsV1::Delta,
            ProviderUsageCountersV1::Unknown {
                reason: tracedecay_domain::CanonicalUnknownStateV1::Malformed,
            },
        ),
        observation(
            2,
            0,
            "codex",
            "codex-session",
            ProviderUsageCounterSemanticsV1::Unknown,
            counters(5, 7),
        ),
    ]);

    assert_eq!(aggregate.coverage, ProviderUsageCoverageV1::Partial);
    assert_eq!(
        aggregate.totals,
        AggregatedProviderUsageCountersV1::unknown()
    );
    assert_eq!(
        aggregate
            .issues
            .iter()
            .filter(|issue| issue.kind == ProviderUsageIssueKindV1::MalformedCounters)
            .count(),
        1
    );
    assert!(
        aggregate
            .issues
            .iter()
            .any(|issue| { issue.kind == ProviderUsageIssueKindV1::UnknownCounterSemantics })
    );
}

#[test]
fn published_known_empty_page_is_complete_and_prices_to_zero() {
    let mut scan = ProviderUsageScanV1::new();
    let aggregate = match scan.accept(ProviderUsageReadV1::Known {
        observations: Vec::new(),
        upper_observation_sequence: 7,
        next_cursor: None,
    }) {
        ScanStep::Complete(aggregate) => aggregate,
        other => panic!("unexpected known-empty scan outcome: {other:?}"),
    };

    assert_eq!(aggregate.coverage, ProviderUsageCoverageV1::Complete);
    assert_eq!(aggregate.observations_seen, 0);
    assert!(aggregate.deltas.is_empty());
    assert!(aggregate.issues.is_empty());
    assert_eq!(aggregate.upper_observation_sequence, Some(7));

    let prices = PriceTable {
        models: BTreeMap::new(),
        available: true,
        source: "fixture",
        revision: "sha256:fixture".to_owned(),
    };
    let summary = price_provider_usage(&aggregate, &prices, 0);
    assert_eq!(summary.coverage, ProviderUsageCoverageV1::Complete);
    assert_eq!(summary.usage_events, 0);
    assert_eq!(summary.unpriced_events, 0);
    assert_eq!(summary.total_cost_usd, Some(0.0));
}

#[test]
fn missing_checkpoint_read_remains_unavailable_and_unpriced() {
    let mut scan = ProviderUsageScanV1::new();
    let aggregate = match scan.accept(ProviderUsageReadV1::Unknown {
        reason: tracedecay_domain::CanonicalUnknownStateV1::Absent,
        upper_observation_sequence: 0,
    }) {
        ScanStep::Complete(aggregate) => aggregate,
        other => panic!("unexpected missing-checkpoint scan outcome: {other:?}"),
    };

    assert_eq!(aggregate.coverage, ProviderUsageCoverageV1::Unavailable);
    assert_eq!(aggregate.observations_seen, 0);
    assert_eq!(
        aggregate.issues[0].kind,
        ProviderUsageIssueKindV1::ReadUnknown
    );

    let prices = PriceTable {
        models: BTreeMap::new(),
        available: true,
        source: "fixture",
        revision: "sha256:fixture".to_owned(),
    };
    let summary = price_provider_usage(&aggregate, &prices, 0);
    assert_eq!(summary.coverage, ProviderUsageCoverageV1::Unavailable);
    assert_eq!(summary.usage_events, 0);
    assert_eq!(summary.total_cost_usd, None);
}

#[test]
fn interrupted_second_page_reports_partial_lower_bound_not_complete() {
    let mut scan = ProviderUsageScanV1::new();
    let cursor = ProviderUsageCursorV1 {
        observation_sequence: 1,
        usage_ordinal: 0,
        upper_observation_sequence: 2,
        scope: ObservationScopeV1::Profile,
        provider: None,
        session_id: None,
    };
    assert_eq!(
        scan.accept(ProviderUsageReadV1::Known {
            observations: vec![observation(
                1,
                0,
                "claude",
                "claude-session",
                ProviderUsageCounterSemanticsV1::Delta,
                counters(11, 3),
            )],
            upper_observation_sequence: 2,
            next_cursor: Some(cursor.clone()),
        }),
        ScanStep::Continue(cursor)
    );

    let aggregate = scan.fail();
    assert_eq!(aggregate.coverage, ProviderUsageCoverageV1::Partial);
    assert_eq!(aggregate.totals, totals(11, 3));
    assert!(
        aggregate
            .issues
            .iter()
            .any(|issue| { issue.kind == ProviderUsageIssueKindV1::PaginationInterrupted })
    );
    assert_eq!(aggregate.upper_observation_sequence, Some(2));
}

#[test]
fn all_provider_pricing_is_exact_and_unknown_models_remain_unpriced() {
    let prices = PriceTable {
        models: BTreeMap::from([
            (
                "anthropic/claude-test".to_owned(),
                ModelPrice {
                    prompt_per_mtok: 3.0,
                    completion_per_mtok: 15.0,
                    cache_read_per_mtok: Some(0.3),
                    cache_write_per_mtok: Some(3.75),
                },
            ),
            (
                "openai/gpt-test".to_owned(),
                ModelPrice {
                    prompt_per_mtok: 2.0,
                    completion_per_mtok: 8.0,
                    cache_read_per_mtok: Some(0.2),
                    cache_write_per_mtok: Some(2.5),
                },
            ),
        ]),
        available: true,
        source: "fixture",
        revision: "sha256:fixture".to_owned(),
    };
    let mut claude = observation(
        1,
        0,
        "claude",
        "claude-session",
        ProviderUsageCounterSemanticsV1::Delta,
        counters(1_000_000, 100_000),
    );
    claude.model = ProviderUsageModelV1::Known {
        model: "claude-test".to_owned(),
    };
    claude.counters = ProviderUsageCountersV1::Known {
        input_tokens: Some(1_000_000),
        output_tokens: Some(100_000),
        cache_read_tokens: Some(0),
        cache_write_tokens: Some(0),
        reasoning_tokens: None,
        total_tokens: Some(1_100_000),
    };
    let mut codex = observation(
        2,
        0,
        "codex",
        "codex-session",
        ProviderUsageCounterSemanticsV1::Delta,
        counters(1_000_000, 100_000),
    );
    codex.model = ProviderUsageModelV1::Known {
        model: "gpt-test".to_owned(),
    };
    codex.counters = ProviderUsageCountersV1::Known {
        input_tokens: Some(1_000_000),
        output_tokens: Some(100_000),
        cache_read_tokens: Some(0),
        cache_write_tokens: Some(0),
        reasoning_tokens: None,
        total_tokens: Some(1_100_000),
    };

    let aggregate = reduce_provider_usage(&[claude, codex]);
    let summary = price_provider_usage(&aggregate, &prices, 0);

    assert_eq!(summary.coverage, ProviderUsageCoverageV1::Complete);
    assert_eq!(summary.usage_events, 2);
    assert_eq!(summary.unpriced_events, 0);
    assert!(
        summary
            .total_cost_usd
            .is_some_and(|cost| (cost - 7.3).abs() < 1e-9)
    );
    assert_eq!(summary.by_model.len(), 2);

    let mut unknown = observation(
        3,
        0,
        "codex",
        "unknown-session",
        ProviderUsageCounterSemanticsV1::Delta,
        counters(10, 2),
    );
    unknown.model = ProviderUsageModelV1::Unknown {
        reason: tracedecay_domain::CanonicalUnknownStateV1::Absent,
    };
    let aggregate = reduce_provider_usage(&[unknown]);
    let summary = price_provider_usage(&aggregate, &prices, 0);
    assert_eq!(summary.coverage, ProviderUsageCoverageV1::Partial);
    assert_eq!(summary.total_cost_usd, None);
    assert_eq!(summary.unpriced_events, 1);
}

#[test]
fn provider_usage_range_rejects_unknown_aliases() {
    assert_eq!(
        super::provider_usage_range_start("yesterday"),
        Err("unsupported provider usage range: yesterday".to_owned())
    );
}
