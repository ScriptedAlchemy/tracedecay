import { describe, expect, it } from 'vitest';
import type { MetricValueV1, ObservatoryReadModelV1 } from '../../contracts/generated.ts';
import { adoptionCoverageBands } from './adoptionCoverage.ts';
import { adoptionOutcomeBands } from './adoptionOutcomes.ts';
import { analyticsModeReading, egressFailureReading, shareStagingReading } from './analyticsControls.ts';
import { performanceBudgetBands } from './performanceBudgets.ts';
import { performanceComparisonBands } from './performanceComparisons.ts';
import { retrievalQualityBands } from './retrievalQuality.ts';

describe('Plan 26 Observatory projection binding', () => {
  it('resolves every numeric panel dimension against the canonical daemon projection', () => {
    const model = fixture();
    const dimensions = [
      ...adoptionCoverageBands(model),
      ...adoptionOutcomeBands(model),
      ...retrievalQualityBands(model),
      ...performanceBudgetBands(model),
      ...performanceComparisonBands(model),
    ].flatMap((band) => band.dimensions);

    expect(dimensions).toHaveLength(59);
    expect(dimensions.filter((dimension) => dimension.reading.kind === 'unpublished')).toEqual([]);
  });

  it('reads controls from the observatory payload without inferring absent values', () => {
    const model = fixture();
    expect(analyticsModeReading(model.analytics_mode)).toMatchObject({
      mode: 'local_only',
      state: 'ready',
    });
    expect(shareStagingReading(model.metrics)).toMatchObject({ ageSeconds: 17, state: 'ready' });
    expect(egressFailureReading(model.metrics)).toMatchObject({
      failures: null,
      state: 'unknown',
    });
  });
});

function fixture(): ObservatoryReadModelV1 {
  const names = [
    'feedback_coverage',
    'feedback_denial_rate',
    'feedback_staleness_rate',
    'feedback_diversity',
    'feedback_omission_rate',
    'feedback_revocation_propagation_p95',
    'observability_eligible_events',
    'observability_events',
    'observability_late_arrivals',
    'telemetry_drops_lower_bound',
    'observability_failures',
    'adoption_eligible',
    'adoption_enabled',
    'adoption_available',
    'adoption_invoked',
    'adoption_terminal',
    'adoption_independently_useful',
    'adoption_repeat_useful',
    'adoption_correct_abstention',
    'adoption_censored_outcomes',
    'adoption_unknown_outcomes',
    'retriever_consumed_candidates',
    'retriever_returned_candidates',
    'retriever_candidate_rank',
    'retriever_unique_contributions',
    'retrieval_planner_span_p95',
    'retrieval_fanout_span_p95',
    'retrieval_synthesis_span_p95',
    'retrieval_context_precision',
    'retrieval_task_outcome_linkage',
    'retrieval_equal_budget_ablation',
    'operation_latency_p50',
    'operation_latency_p95',
    'operation_latency_p99',
    'queue_span_p95',
    'store_lock_span_p95',
    'index_lock_span_p95',
    'provider_negotiation_span_p95',
    'process_rss_peak',
    'cpu_time_total',
    'io_amplification',
    'no_progress_outcomes',
    'accepted_budget_revision',
    'comparison_baseline_build',
    'comparison_candidate_build',
    'comparison_workload_corpus',
    'comparison_environment_platform',
    'comparison_oracle',
    'comparison_rollback_profile',
    'comparison_outcome_counts',
    'comparison_stratum_support',
    'comparison_intervals',
    'comparison_calibration',
    'comparison_risk_coverage',
    'comparison_flaky_indeterminate',
    'comparison_deviations',
    'comparison_paired_outcomes',
    'analytics_share_staging_age_seconds',
    'analytics_egress_failures',
  ];
  return {
    authorized_scope_ref: 'project:test',
    horizon: { since_micros: 1, until_micros: 2 },
    watermark: 'analytics:7',
    observed_at_micros: 2,
    current: false,
    metrics: names.map((name) => metric(name, name === 'analytics_share_staging_age_seconds' ? 17 : null)),
    analytics_mode: {
      current: 'local_only',
      transition_watermark: 'producer:7',
      coverage: coverage('known'),
      unavailable_reason: null,
    },
    comparison: {
      baseline_build: null,
      candidate_build: null,
      workload: null,
      corpus: null,
      environment: null,
      oracle: null,
      configuration: null,
      platform: null,
      rollback_profile: null,
      eligible_outcomes: null,
      paired_outcomes: null,
      regression_observed: null,
      disposition: 'insufficient_evidence',
      coverage: coverage('unknown'),
      unavailable_reason: 'comparison_evidence_not_recorded',
    },
    rejected_arguments: {
      coverage: coverage('unknown'),
      projector_revision: 'observatory-rejected-argument-projector.v1',
      watermark: 'analytics:4821',
      eligible_attempts: null,
      rejected_total: null,
      rejection_rate: null,
      redacted_name_count: 0,
      groups: [],
      unavailable_reason: 'rejected_argument_observations_not_recorded',
    },
  } as ObservatoryReadModelV1;
}

function coverage(state: 'known' | 'unknown') {
  return {
    eligible: state === 'known' ? 1 : null,
    observed: state === 'known' ? 1 : 0,
    completed: state === 'known' ? 1 : 0,
    censored: 0,
    unknown: state === 'known' ? 0 : 1,
    excluded: 0,
    state,
  };
}

function metric(name: string, value: number | null): MetricValueV1 {
  return {
    descriptor_revision: 'plan26.v1',
    metric: name,
    value,
    unit: name.includes('latency') || name.includes('span') ? 'microseconds' : 'events',
    denominator: 'eligible_observations',
    denominator_value: value == null ? null : 1,
    coverage: coverage(value == null ? 'unknown' : 'known'),
    evidence_class: 'measurement',
    provenance: {
      source: 'observability_envelope',
      source_revision: 'observability-envelope.v1',
      projector_revision: 'observatory-plan26-projector.v1',
      watermark: 'analytics:7',
    },
    cohort: { descriptor_revision: 'eligible.v1', eligible_population: 'eligible_observations' },
    temporal: {
      horizon: { since_micros: 1, until_micros: 2 },
      baseline_watermark: null,
      delta: null,
    },
    uncertainty: { lower: value, upper: value, reason: value == null ? 'not_observed' : null },
    calibration: null,
    unavailable_reason: value == null ? 'not_observed' : null,
  };
}
