//! Provider latency projection for the canonical Costs read model.
//!
//! Latency is measured only from retained `operation.resource.completed.v1`
//! envelopes. Provider/model identity is joined only when the envelope's
//! payload's explicit provider-native request identity exactly matches a
//! request identity in the immutable provider-usage projection. A generated
//! envelope trace identity, time-adjacent row, session-wide row, client
//! stopwatch, or model default is never an attribution source.

use std::collections::{BTreeMap, BTreeSet};

use tracedecay_application::{
    LatencyDistributionReadModelV1, MetricCoverageV1, MetricProvenanceV1, MetricSourceV1,
    MetricValueV1, ObservabilityHorizonV1, ProviderLatencyReadModelV1,
};
use tracedecay_domain::{
    CoverageStateV1, ObservabilityEnvelopeV1, ObservabilityPayloadV1, ObservationScopeV1,
    OperationResourceObservedV1, OperationStageV1,
};
use tracedecay_global_db::RegisteredGlobalDb;

use super::{
    EVENT_LIMIT, MeasurementDescriptor, MeasurementProvenance, MeasurementSpec, coverage,
    measurement,
};
use crate::observability::RegisteredObservabilityPortV1;
use crate::provider_usage::{
    ProviderUsageAggregateV1, ProviderUsageCoverageV1, ProviderUsageDeltaV1,
};
use tracedecay_application::{ObservabilityQueryPort, ObservabilityQueryV1};

const LATENCY_DESCRIPTOR: &str = "provider-latency.v1";
const LATENCY_PROJECTOR: &str = "costs-provider-latency-projector.v1";
const OPERATION_SOURCE_REVISION: &str = "operation-resource-observation.v1";
const PROVIDER_USAGE_SOURCE_REVISION: &str = "provider-usage-observation.v1";
const UNKNOWN_IDENTITY_REASON: &str = "provider_model_identity_unavailable";
const NO_LATENCY_REASON: &str = "provider_operation_resources_not_recorded";
const INCOMPLETE_REASON: &str = "incomplete_operation_resource_coverage";
const CENSORED_REASON: &str = "latency_censored_or_missing";

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum StageMetric {
    Queue,
    Start,
    FirstProgress,
    Service,
    Terminal,
}

impl StageMetric {
    const fn label(self) -> &'static str {
        match self {
            Self::Queue => "queue",
            Self::Start => "start",
            Self::FirstProgress => "first_progress",
            Self::Service => "service",
            Self::Terminal => "terminal",
        }
    }
}

#[derive(Clone, Debug)]
struct Identity {
    provider: Option<String>,
    model: Option<String>,
    source: MetricSourceV1,
    unavailable_reason: Option<&'static str>,
}

#[derive(Clone, Debug)]
struct Sample {
    values: BTreeMap<StageMetric, u64>,
    censored: BTreeSet<StageMetric>,
}

#[derive(Default)]
struct Group {
    samples: Vec<Sample>,
    identity_sources: Vec<MetricSourceV1>,
    identity_reason: Option<&'static str>,
}

/// Projects provider/model latency from the canonical Plan 26 event family.
pub async fn provider_latency_read_model(
    db: Option<&RegisteredGlobalDb>,
    scope_ref: Option<&str>,
    horizon: &ObservabilityHorizonV1,
    provider_usage: &ProviderUsageAggregateV1,
) -> Vec<ProviderLatencyReadModelV1> {
    let Some(db) = db else {
        return vec![unavailable_provider_latency(
            horizon,
            "observability_store_unavailable",
        )];
    };
    let Some(scope_ref) = scope_ref else {
        return vec![unavailable_provider_latency(
            horizon,
            "provider_latency_scope_unavailable",
        )];
    };
    let page = RegisteredObservabilityPortV1::new(db)
        .query(ObservabilityQueryV1 {
            authorized_scope_ref: scope_ref.to_owned(),
            event_kinds: vec!["operation.resource.completed.v1".to_owned()],
            horizon: horizon.clone(),
            after_watermark: None,
            limit: EVENT_LIMIT.min(u32::MAX as usize) as u32,
        })
        .await;
    let Ok(page) = page else {
        return vec![unavailable_provider_latency(
            horizon,
            "observability_store_unavailable",
        )];
    };
    // The bounded producer carries queue gaps on the next accepted envelope,
    // and also seals a standalone telemetry-drop receipt at shutdown. Read the
    // same canonical drop family so an operation page cannot look complete
    // merely because a dropped operation never reached this query.
    let drop_page = RegisteredObservabilityPortV1::new(db)
        .query(ObservabilityQueryV1 {
            authorized_scope_ref: scope_ref.to_owned(),
            event_kinds: vec!["telemetry.drop.observed.v1".to_owned()],
            horizon: horizon.clone(),
            after_watermark: None,
            limit: EVENT_LIMIT.min(u32::MAX as usize) as u32,
        })
        .await;
    let (drop_coverage, drop_watermark) = match drop_page {
        Ok(page) => {
            let receipt_coverage =
                page.events
                    .iter()
                    .fold(CoverageStateV1::Known, |state, event| {
                        let ObservabilityPayloadV1::TelemetryDrop(drop) = &event.payload else {
                            return state;
                        };
                        let receipt_state = if drop.proved_drop_lower_bound > 0 {
                            CoverageStateV1::Partial
                        } else if !drop.clean_shutdown_observed {
                            CoverageStateV1::Unknown
                        } else {
                            CoverageStateV1::Known
                        };
                        weaker_coverage(state, receipt_state)
                    });
            (
                weaker_coverage(page.coverage, receipt_coverage),
                page.watermark,
            )
        }
        Err(_) => (
            CoverageStateV1::Unknown,
            "analytics:drop-unavailable".to_owned(),
        ),
    };
    let carried_drop = page.events.iter().any(|event| {
        matches!(&event.payload, ObservabilityPayloadV1::OperationResource(_))
            && (event.coverage != CoverageStateV1::Known || event.dropped_count > 0)
    });
    let mut source_coverage = weaker_coverage(page.coverage, drop_coverage);
    if carried_drop {
        source_coverage = weaker_coverage(source_coverage, CoverageStateV1::Partial);
    }
    let complete = source_coverage == CoverageStateV1::Known;
    let watermark = format!("{};{}", page.watermark, drop_watermark);
    let mut groups: BTreeMap<(Option<String>, Option<String>), Group> = BTreeMap::new();
    for envelope in &page.events {
        let ObservabilityPayloadV1::OperationResource(resource) = &envelope.payload else {
            continue;
        };
        let identity = resolve_identity(
            &envelope.scope_ref,
            resource.provider_request_id.as_deref(),
            provider_usage,
            horizon,
        );
        let key = (identity.provider.clone(), identity.model.clone());
        let group = groups.entry(key).or_default();
        if !group.identity_sources.contains(&identity.source) {
            group.identity_sources.push(identity.source);
        }
        if identity.unavailable_reason.is_some() {
            group.identity_reason = identity.unavailable_reason;
        }
        group.samples.push(sample(envelope, resource));
    }
    if groups.is_empty() {
        return vec![if source_coverage == CoverageStateV1::Known {
            unavailable_provider_latency(horizon, NO_LATENCY_REASON)
        } else {
            unavailable_provider_latency_with_coverage(
                horizon,
                INCOMPLETE_REASON,
                source_coverage,
                &watermark,
            )
        }];
    }
    groups
        .into_iter()
        .map(|((provider, model), group)| {
            project_group(
                provider,
                model,
                group,
                horizon,
                &watermark,
                complete,
                source_coverage,
            )
        })
        .collect()
}

pub fn unavailable_provider_latency(
    horizon: &ObservabilityHorizonV1,
    reason: &str,
) -> ProviderLatencyReadModelV1 {
    unavailable_provider_latency_with_coverage(
        horizon,
        reason,
        CoverageStateV1::Unknown,
        "analytics:unavailable",
    )
}

fn unavailable_provider_latency_with_coverage(
    horizon: &ObservabilityHorizonV1,
    reason: &str,
    state: CoverageStateV1,
    watermark: &str,
) -> ProviderLatencyReadModelV1 {
    let identity_provenance = MetricProvenanceV1 {
        source: MetricSourceV1::ObservabilityEnvelope,
        source_revision: OPERATION_SOURCE_REVISION.to_owned(),
        projector_revision: LATENCY_PROJECTOR.to_owned(),
        watermark: watermark.to_owned(),
    };
    let distribution = |stage: StageMetric| LatencyDistributionReadModelV1 {
        p50: unknown_metric_with_coverage(stage, 50, horizon, reason, state, watermark),
        p95: unknown_metric_with_coverage(stage, 95, horizon, reason, state, watermark),
        p99: unknown_metric_with_coverage(stage, 99, horizon, reason, state, watermark),
    };
    ProviderLatencyReadModelV1 {
        provider: None,
        model: None,
        identity_provenance,
        identity_unavailable_reason: Some(reason.to_owned()),
        queue: distribution(StageMetric::Queue),
        start: distribution(StageMetric::Start),
        first_progress: distribution(StageMetric::FirstProgress),
        service: distribution(StageMetric::Service),
        terminal: distribution(StageMetric::Terminal),
    }
}

fn weaker_coverage(left: CoverageStateV1, right: CoverageStateV1) -> CoverageStateV1 {
    fn rank(state: CoverageStateV1) -> u8 {
        match state {
            CoverageStateV1::Known => 0,
            CoverageStateV1::Sampled => 1,
            CoverageStateV1::Capped => 2,
            CoverageStateV1::Partial => 3,
            CoverageStateV1::Stale => 4,
            CoverageStateV1::Unknown => 5,
        }
    }
    if rank(left) >= rank(right) {
        left
    } else {
        right
    }
}

fn resolve_identity(
    scope_ref: &str,
    provider_request_id: Option<&str>,
    provider_usage: &ProviderUsageAggregateV1,
    horizon: &ObservabilityHorizonV1,
) -> Identity {
    let candidates = matching_usage(scope_ref, provider_request_id, provider_usage, horizon);
    let provider = unique_text(candidates.iter().map(|delta| delta.provider.as_str()));
    let model = unique_optional_text(candidates.iter().map(|delta| delta.model.as_deref()));
    let identity_complete = provider.is_some() && model.is_some();
    Identity {
        provider,
        model,
        source: if candidates.is_empty() {
            MetricSourceV1::ObservabilityEnvelope
        } else {
            MetricSourceV1::ProviderUsageObservation
        },
        unavailable_reason: (!identity_complete).then_some(UNKNOWN_IDENTITY_REASON),
    }
}

fn matching_usage<'a>(
    scope_ref: &str,
    provider_request_id: Option<&str>,
    provider_usage: &'a ProviderUsageAggregateV1,
    horizon: &ObservabilityHorizonV1,
) -> Vec<&'a ProviderUsageDeltaV1> {
    let Some(provider_request_id) = provider_request_id else {
        return Vec::new();
    };
    if provider_usage.coverage != ProviderUsageCoverageV1::Complete {
        // A partial usage scan cannot prove that the one matching row is the
        // only provider/model candidate. Keep identity unavailable rather
        // than upgrading an incomplete join into a cohort attribution.
        return Vec::new();
    }
    const MICROS_PER_SECOND: i64 = 1_000_000;
    let since_seconds = horizon.since_micros.div_euclid(MICROS_PER_SECOND);
    let until_seconds = horizon
        .until_micros
        .saturating_add(MICROS_PER_SECOND - 1)
        .div_euclid(MICROS_PER_SECOND);
    let same_scope = |delta: &&ProviderUsageDeltaV1| {
        matches!(
            &delta.scope,
            ObservationScopeV1::Project { project_id } if project_id.as_str() == scope_ref
        ) && delta
            .native_timestamp
            .is_some_and(|timestamp| timestamp >= since_seconds && timestamp < until_seconds)
    };
    provider_usage
        .deltas
        .iter()
        .filter(same_scope)
        .filter(|delta| {
            delta.request_id.as_deref() == Some(provider_request_id)
                || delta.turn_id.as_deref() == Some(provider_request_id)
        })
        .collect()
}

fn unique_text<'a>(values: impl Iterator<Item = &'a str>) -> Option<String> {
    let values = values.map(str::to_owned).collect::<BTreeSet<_>>();
    (values.len() == 1)
        .then(|| values.into_iter().next())
        .flatten()
}

fn unique_optional_text<'a>(values: impl Iterator<Item = Option<&'a str>>) -> Option<String> {
    let values = values
        .map(|value| value.map(str::to_owned))
        .collect::<Option<Vec<_>>>()?;
    let values = values.into_iter().collect::<BTreeSet<_>>();
    (values.len() == 1)
        .then(|| values.into_iter().next())
        .flatten()
}

fn sample(envelope: &ObservabilityEnvelopeV1, resource: &OperationResourceObservedV1) -> Sample {
    let mut values = BTreeMap::new();
    let mut censored = BTreeSet::new();
    let scheduled = stage_elapsed(resource, OperationStageV1::Scheduled);
    let admitted = stage_elapsed(resource, OperationStageV1::Admitted);
    let started = stage_elapsed(resource, OperationStageV1::Started);
    if let (Some(scheduled), Some(admitted)) = (scheduled, admitted) {
        values.insert(StageMetric::Queue, admitted.saturating_sub(scheduled));
    } else {
        // The canonical summary carries the scheduled-arrival span even when
        // detailed stage timings were not retained.
        values.insert(StageMetric::Queue, resource.scheduled_latency_micros);
    }
    if let (Some(scheduled), Some(started)) = (scheduled, started) {
        values.insert(StageMetric::Start, started.saturating_sub(scheduled));
    }
    if let Some(first_progress) = stage_elapsed(resource, OperationStageV1::FirstProgress) {
        values.insert(StageMetric::FirstProgress, first_progress);
    } else if envelope.terminal_result.is_none()
        || matches!(
            envelope.terminal_result,
            Some(
                tracedecay_domain::ObservabilityTerminalResultV1::TimedOut
                    | tracedecay_domain::ObservabilityTerminalResultV1::Cancelled
            )
        )
    {
        censored.insert(StageMetric::FirstProgress);
    }
    if started.is_some() {
        values.insert(StageMetric::Service, resource.service_latency_micros);
    } else {
        // A launch/effect refusal can have a terminal receipt without ever
        // entering provider service. Keep that denominator censored rather
        // than exposing the payload's mandatory zero as a real duration.
        censored.insert(StageMetric::Service);
    }
    if let Some(terminal) = stage_elapsed(resource, OperationStageV1::Terminal) {
        values.insert(StageMetric::Terminal, terminal);
    } else if envelope.terminal_result.is_none() {
        censored.insert(StageMetric::Terminal);
    }
    Sample { values, censored }
}

fn stage_elapsed(resource: &OperationResourceObservedV1, stage: OperationStageV1) -> Option<u64> {
    resource
        .stage_timings
        .iter()
        .find(|timing| timing.stage == stage)
        .map(|timing| timing.elapsed_micros)
}

fn project_group(
    provider: Option<String>,
    model: Option<String>,
    group: Group,
    horizon: &ObservabilityHorizonV1,
    watermark: &str,
    source_complete: bool,
    source_coverage: CoverageStateV1,
) -> ProviderLatencyReadModelV1 {
    let identity_source = if group.identity_sources.len() == 1 {
        group
            .identity_sources
            .first()
            .copied()
            .unwrap_or(MetricSourceV1::ObservabilityEnvelope)
    } else {
        MetricSourceV1::ObservabilityEnvelope
    };
    let identity_provenance = MetricProvenanceV1 {
        source: identity_source,
        source_revision: if identity_source == MetricSourceV1::ProviderUsageObservation {
            PROVIDER_USAGE_SOURCE_REVISION.to_owned()
        } else {
            OPERATION_SOURCE_REVISION.to_owned()
        },
        projector_revision: LATENCY_PROJECTOR.to_owned(),
        watermark: watermark.to_owned(),
    };
    let distribution = |stage: StageMetric| {
        project_distribution(
            stage,
            &group.samples,
            horizon,
            watermark,
            source_complete,
            source_coverage,
        )
    };
    ProviderLatencyReadModelV1 {
        provider,
        model,
        identity_provenance,
        identity_unavailable_reason: group.identity_reason.map(str::to_owned),
        queue: distribution(StageMetric::Queue),
        start: distribution(StageMetric::Start),
        first_progress: distribution(StageMetric::FirstProgress),
        service: distribution(StageMetric::Service),
        terminal: distribution(StageMetric::Terminal),
    }
}

fn project_distribution(
    stage: StageMetric,
    samples: &[Sample],
    horizon: &ObservabilityHorizonV1,
    watermark: &str,
    source_complete: bool,
    source_coverage: CoverageStateV1,
) -> LatencyDistributionReadModelV1 {
    let eligible = samples.len() as u64;
    let censored = samples
        .iter()
        .filter(|sample| sample.censored.contains(&stage))
        .count() as u64;
    let observed_values = samples
        .iter()
        .filter_map(|sample| sample.values.get(&stage).copied())
        .collect::<Vec<_>>();
    let observed = observed_values.len() as u64;
    let unknown = eligible.saturating_sub(observed).saturating_sub(censored);
    let complete = source_complete && eligible > 0 && censored == 0 && unknown == 0;
    let state = if !source_complete {
        source_coverage
    } else if complete {
        CoverageStateV1::Known
    } else {
        CoverageStateV1::Partial
    };
    let reason = if !source_complete {
        Some(INCOMPLETE_REASON)
    } else if eligible == 0 {
        Some(NO_LATENCY_REASON)
    } else if !complete {
        Some(CENSORED_REASON)
    } else {
        None
    };
    let coverage = MetricCoverageV1 {
        eligible: source_complete.then_some(eligible),
        // The denominator is unavailable under a capped/drop-tainted source,
        // but retained samples are still an honest lower bound. Preserve
        // observed/completed/censored counts and add one unknown slot so a
        // partial page cannot be mistaken for a complete census.
        observed,
        completed: observed,
        censored,
        unknown: if source_complete {
            unknown
        } else {
            unknown.max(1)
        },
        excluded: 0,
        state,
    };
    let value = |percentile_rank| {
        let value = complete.then(|| percentile(&observed_values, percentile_rank));
        metric(
            stage,
            percentile_rank,
            value.flatten().map(|value| value as f64),
            coverage.clone(),
            horizon,
            watermark,
            reason,
        )
    };
    LatencyDistributionReadModelV1 {
        p50: value(50),
        p95: value(95),
        p99: value(99),
    }
}

fn percentile(values: &[u64], rank: usize) -> Option<u64> {
    let mut values = values.to_vec();
    values.sort_unstable();
    let index = values
        .len()
        .checked_mul(rank)
        .and_then(|value| value.checked_add(99))
        .map(|value| value / 100)
        .and_then(|rank| rank.checked_sub(1))?;
    values.get(index).copied()
}

fn metric(
    stage: StageMetric,
    percentile_rank: usize,
    value: Option<f64>,
    coverage: MetricCoverageV1,
    horizon: &ObservabilityHorizonV1,
    watermark: &str,
    reason: Option<&str>,
) -> MetricValueV1 {
    measurement(MeasurementSpec {
        descriptor: MeasurementDescriptor::new(
            LATENCY_DESCRIPTOR,
            &format!("provider_{}_latency_p{}", stage.label(), percentile_rank),
            "microseconds",
            "provider_operation_resource_observations",
        ),
        provenance: MeasurementProvenance::new(
            MetricSourceV1::ObservabilityEnvelope,
            OPERATION_SOURCE_REVISION,
            LATENCY_PROJECTOR,
            watermark,
        ),
        horizon,
        coverage,
        value,
        unavailable_reason: reason,
    })
}

fn unknown_metric_with_coverage(
    stage: StageMetric,
    percentile_rank: usize,
    horizon: &ObservabilityHorizonV1,
    reason: &str,
    state: CoverageStateV1,
    watermark: &str,
) -> MetricValueV1 {
    let coverage = coverage(None, 0, 1, state);
    metric(
        stage,
        percentile_rank,
        None,
        coverage,
        horizon,
        watermark,
        Some(reason),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_usage::{
        AggregatedProviderUsageCountersV1, ProviderUsageDeltaDerivationV1,
    };

    const SCOPE_REF: &str =
        "sha256:0000000000000000000000000000000000000000000000000000000000000000";

    fn horizon() -> ObservabilityHorizonV1 {
        ObservabilityHorizonV1 {
            since_micros: 0,
            until_micros: 100,
        }
    }

    fn usage_horizon() -> ObservabilityHorizonV1 {
        ObservabilityHorizonV1 {
            since_micros: 0,
            until_micros: 2_000_000,
        }
    }

    fn sample_with(values: &[(StageMetric, u64)], censored: &[StageMetric]) -> Sample {
        Sample {
            values: values.iter().copied().collect(),
            censored: censored.iter().copied().collect(),
        }
    }

    fn usage_delta(
        provider: &str,
        model: Option<&str>,
        session_id: &str,
        request_id: Option<&str>,
        timestamp: i64,
        sequence: u64,
    ) -> ProviderUsageDeltaV1 {
        ProviderUsageDeltaV1 {
            observation_id: format!("observation:{sequence}"),
            receipt_id: format!("receipt:{sequence}"),
            observation_sequence: sequence,
            usage_ordinal: 0,
            scope: ObservationScopeV1::Project {
                project_id: tracedecay_domain::ProjectId::new(SCOPE_REF).unwrap(),
            },
            provider: provider.to_owned(),
            model: model.map(str::to_owned),
            session_id: session_id.to_owned(),
            turn_id: None,
            message_id: None,
            request_id: request_id.map(str::to_owned),
            native_kind: "usage".to_owned(),
            native_field: "tokens".to_owned(),
            native_timestamp: Some(timestamp),
            derivation: ProviderUsageDeltaDerivationV1::NativeDelta,
            derived_from_sequence: None,
            counters: AggregatedProviderUsageCountersV1::unknown(),
        }
    }

    fn complete_usage(deltas: Vec<ProviderUsageDeltaV1>) -> ProviderUsageAggregateV1 {
        ProviderUsageAggregateV1 {
            coverage: ProviderUsageCoverageV1::Complete,
            observations_seen: deltas.len() as u64,
            totals: AggregatedProviderUsageCountersV1::unknown(),
            deltas,
            issues: Vec::new(),
            upper_observation_sequence: Some(1),
        }
    }

    #[test]
    fn exact_request_join_supplies_missing_model_identity() {
        let aggregate = complete_usage(vec![usage_delta(
            "codex",
            Some("gpt-test"),
            "session-1",
            Some("request-1"),
            1,
            1,
        )]);
        let identity = resolve_identity(SCOPE_REF, Some("request-1"), &aggregate, &usage_horizon());
        assert_eq!(identity.provider.as_deref(), Some("codex"));
        assert_eq!(identity.model.as_deref(), Some("gpt-test"));
        assert_eq!(identity.source, MetricSourceV1::ProviderUsageObservation);
        assert_eq!(identity.unavailable_reason, None);
    }

    #[test]
    fn exact_turn_join_supplies_codex_model_identity() {
        let mut delta = usage_delta("codex", Some("gpt-test"), "session-1", None, 1, 1);
        delta.turn_id = Some("turn-1".to_owned());
        let aggregate = complete_usage(vec![delta]);

        let identity = resolve_identity(SCOPE_REF, Some("turn-1"), &aggregate, &usage_horizon());

        assert_eq!(identity.provider.as_deref(), Some("codex"));
        assert_eq!(identity.model.as_deref(), Some("gpt-test"));
        assert_eq!(identity.source, MetricSourceV1::ProviderUsageObservation);
        assert_eq!(identity.unavailable_reason, None);
    }

    #[test]
    fn ambiguous_request_join_keeps_identity_unavailable() {
        let aggregate = complete_usage(vec![
            usage_delta("codex", Some("gpt-a"), "session-1", Some("request-1"), 1, 1),
            usage_delta("codex", Some("gpt-b"), "session-1", Some("request-1"), 1, 2),
        ]);
        let identity = resolve_identity(SCOPE_REF, Some("request-1"), &aggregate, &usage_horizon());
        assert_eq!(identity.provider.as_deref(), Some("codex"));
        assert_eq!(identity.model, None);
        assert_eq!(identity.unavailable_reason, Some(UNKNOWN_IDENTITY_REASON));
    }

    #[test]
    fn partial_usage_never_upgrades_an_exact_identity_join() {
        let mut aggregate = complete_usage(vec![usage_delta(
            "codex",
            Some("gpt-test"),
            "session-1",
            Some("request-1"),
            1,
            1,
        )]);
        aggregate.coverage = ProviderUsageCoverageV1::Partial;
        let identity = resolve_identity(SCOPE_REF, Some("request-1"), &aggregate, &usage_horizon());
        assert_eq!(identity.provider, None);
        assert_eq!(identity.model, None);
        assert_eq!(identity.source, MetricSourceV1::ObservabilityEnvelope);
        assert_eq!(identity.unavailable_reason, Some(UNKNOWN_IDENTITY_REASON));
    }

    #[test]
    fn missing_provider_request_identity_never_joins_a_generated_trace() {
        let aggregate = complete_usage(vec![usage_delta(
            "codex",
            Some("gpt-test"),
            "session-1",
            Some("generated-envelope-trace"),
            1,
            1,
        )]);

        let identity = resolve_identity(SCOPE_REF, None, &aggregate, &usage_horizon());

        assert_eq!(identity.provider, None);
        assert_eq!(identity.model, None);
        assert_eq!(identity.unavailable_reason, Some(UNKNOWN_IDENTITY_REASON));
    }

    #[test]
    fn projects_complete_provider_model_cohort_percentiles() {
        let group = Group {
            samples: vec![
                sample_with(
                    &[
                        (StageMetric::Queue, 10),
                        (StageMetric::Start, 20),
                        (StageMetric::FirstProgress, 30),
                        (StageMetric::Service, 40),
                        (StageMetric::Terminal, 50),
                    ],
                    &[],
                ),
                sample_with(
                    &[
                        (StageMetric::Queue, 15),
                        (StageMetric::Start, 25),
                        (StageMetric::FirstProgress, 35),
                        (StageMetric::Service, 45),
                        (StageMetric::Terminal, 55),
                    ],
                    &[],
                ),
            ],
            identity_sources: vec![MetricSourceV1::ObservabilityEnvelope],
            identity_reason: None,
        };
        let model = project_group(
            Some("codex".to_owned()),
            Some("gpt-test".to_owned()),
            group,
            &horizon(),
            "analytics:12",
            true,
            CoverageStateV1::Known,
        );
        assert_eq!(model.provider.as_deref(), Some("codex"));
        assert_eq!(model.model.as_deref(), Some("gpt-test"));
        assert_eq!(model.queue.p50.value, Some(10.0));
        assert_eq!(model.queue.p95.value, Some(15.0));
        assert_eq!(model.service.p99.value, Some(45.0));
        assert_eq!(model.terminal.p50.coverage.state, CoverageStateV1::Known);
    }

    #[test]
    fn partial_source_preserves_observed_lower_bound_without_percentile_value() {
        let group = Group {
            samples: vec![sample_with(&[(StageMetric::Queue, 10)], &[])],
            identity_sources: vec![MetricSourceV1::ObservabilityEnvelope],
            identity_reason: None,
        };
        let model = project_group(
            Some("codex".to_owned()),
            Some("gpt-test".to_owned()),
            group,
            &horizon(),
            "analytics:12;analytics:drop:7",
            false,
            CoverageStateV1::Partial,
        );
        assert_eq!(model.queue.p50.value, None);
        assert_eq!(model.queue.p50.coverage.eligible, None);
        assert_eq!(model.queue.p50.coverage.observed, 1);
        assert_eq!(model.queue.p50.coverage.unknown, 1);
        assert_eq!(model.queue.p50.coverage.state, CoverageStateV1::Partial);
    }

    #[test]
    fn censored_stage_is_not_rendered_as_zero() {
        let group = Group {
            samples: vec![sample_with(
                &[(StageMetric::Queue, 10), (StageMetric::Service, 40)],
                &[StageMetric::FirstProgress, StageMetric::Terminal],
            )],
            identity_sources: vec![MetricSourceV1::ObservabilityEnvelope],
            identity_reason: None,
        };
        let model = project_group(
            Some("codex".to_owned()),
            Some("gpt-test".to_owned()),
            group,
            &horizon(),
            "analytics:12",
            true,
            CoverageStateV1::Known,
        );
        assert_eq!(model.queue.p50.value, Some(10.0));
        assert_eq!(model.first_progress.p50.value, None);
        assert_eq!(model.first_progress.p50.coverage.censored, 1);
        assert_eq!(model.terminal.p50.value, None);
        assert_eq!(model.terminal.p50.coverage.censored, 1);
    }

    #[test]
    fn drop_only_unavailable_vector_keeps_partial_state_and_reason() {
        let model = unavailable_provider_latency_with_coverage(
            &horizon(),
            INCOMPLETE_REASON,
            CoverageStateV1::Partial,
            "analytics:drop:7",
        );
        assert_eq!(model.queue.p50.value, None);
        assert_eq!(model.queue.p50.coverage.state, CoverageStateV1::Partial);
        assert_eq!(
            model.queue.p50.unavailable_reason.as_deref(),
            Some(INCOMPLETE_REASON)
        );
        assert_eq!(
            model.identity_unavailable_reason.as_deref(),
            Some(INCOMPLETE_REASON)
        );
    }
}
