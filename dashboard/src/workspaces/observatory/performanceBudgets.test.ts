import { describe, expect, it } from 'vitest';
import type { MetricValueV1, ObservatoryReadModelV1 } from '../../contracts/generated.ts';
import { NOT_PUBLISHED, NO_FIGURE, planDimensionPresentation } from './planDimension.ts';
import {
  budgetAnchors,
  budgetCoverage,
  latencyDimensions,
  outcomeDimensions,
  performanceBudgetBands,
  resourceDimensions,
  spanDimensions,
} from './performanceBudgets.ts';

/**
 * `performance-budgets` binds to `/api/observatory`, which carries two of the
 * budget dimensions Plan 26 requires. The tests pin both halves: that the two
 * measured ones read from the wire, and that the eleven unprojected ones stay
 * explicitly unavailable rather than collapsing into zeroes — which is the
 * failure the plan's "unavailable rather than zero" rule names.
 */

const NOW = 1_753_003_600_000_000;

describe('latency dimensions', () => {
  it('reads the p95 the wire publishes', () => {
    const dimensions = latencyDimensions(model([metric('operation_latency_p95', 43_250)]));
    const p95 = dimensions.find((dimension) => dimension.id === 'latency_p95');
    expect(p95?.reading.kind).toBe('measured');
  });

  it('keeps p50 and p99 unpublished rather than repeating the p95 figure', () => {
    // One published percentile must never be printed as three readings.
    const dimensions = latencyDimensions(model([metric('operation_latency_p95', 43_250)]));
    for (const id of ['latency_p50', 'latency_p99']) {
      const dimension = dimensions.find((candidate) => candidate.id === id);
      expect(dimension?.reading.kind).toBe('unpublished');
      expect(dimension?.reading.kind === 'unpublished' && dimension.reading.reason).toContain(
        'no operation-latency evidence',
      );
    }
  });

  it('separates a p95 the projector could not value from a p95 nothing projects', () => {
    const dimensions = latencyDimensions(
      model([
        { ...metric('operation_latency_p95', null), unavailable_reason: 'no_latency_samples' },
      ]),
    );
    const p95 = dimensions.find((dimension) => dimension.id === 'latency_p95');
    expect(p95?.reading.kind).toBe('unmeasured');
    expect(p95?.reading.kind === 'unmeasured' && p95.reading.reason).toBe('no_latency_samples');
  });

  it('never renders an unavailable percentile as zero milliseconds', () => {
    const anchors = budgetAnchors(model([]));
    for (const dimension of latencyDimensions(model([]))) {
      const presented = planDimensionPresentation(dimension, anchors);
      expect(presented.figure).toBe(NO_FIGURE);
      expect(presented.figure).not.toBe('0');
      expect(presented.unit).toBeNull();
    }
  });
});

describe('span, resource, and outcome dimensions', () => {
  it('names each span stage separately so one unavailable stage is visible', () => {
    expect(spanDimensions(model([])).map((dimension) => dimension.id)).toEqual([
      'queue_span',
      'store_lock_span',
      'index_lock_span',
      'provider_negotiation_span',
    ]);
    for (const dimension of spanDimensions(model([]))) {
      expect(dimension.reading.kind).toBe('unpublished');
      expect(dimension.reading.kind === 'unpublished' && dimension.reading.reason).toContain(
        'no span evidence',
      );
    }
  });

  it('keeps RSS, CPU, and I/O as three axes rather than one resource score', () => {
    expect(resourceDimensions(model([])).map((dimension) => dimension.id)).toEqual([
      'process_rss',
      'cpu_time',
      'io_amplification',
    ]);
  });

  it('names the no-progress producer and refuses to invent an accepted budget', () => {
    const outcomes = outcomeDimensions(model([]));
    const noProgress = outcomes.find((dimension) => dimension.id === 'no_progress_outcomes');
    expect(noProgress?.reading.kind === 'unpublished' && noProgress.reading.reason).toContain(
      'no no-progress evidence',
    );
    const revision = outcomes.find((dimension) => dimension.id === 'accepted_budget_revision');
    // A projector revision is not an accepted budget, and the card says so.
    expect(revision?.reading.kind === 'unpublished' && revision.reading.reason).toContain(
      'not an accepted budget',
    );
  });
});

describe('budget bands and coverage', () => {
  it('covers every dimension the plan sentence names', () => {
    const bands = performanceBudgetBands(model([]));
    expect(bands.map((band) => band.marker)).toEqual([
      'latency',
      'spans',
      'resources',
      'outcomes',
    ]);
    expect(bands.flatMap((band) => band.dimensions)).toHaveLength(13);
  });

  it('counts measured dimensions against the required total, not against itself', () => {
    const bands = performanceBudgetBands(
      model([
        metric('operation_latency_p95', 43_250),
        metric('feedback_revocation_propagation_p95', 1_200),
      ]),
    );
    expect(budgetCoverage(bands)).toEqual({ measured: 2, required: 13, unprojected: 11 });
  });

  it('reports nothing measured when the read model carries no measurements', () => {
    const bands = performanceBudgetBands(model([]));
    expect(budgetCoverage(bands).measured).toBe(0);
    // Thirteen requirements, none answered — stated as such, not as a page of
    // zeroes.
    expect(budgetCoverage(bands).unprojected).toBe(13);
  });

  it('states every mandatory card row as not published for an unprojected dimension', () => {
    const anchors = budgetAnchors(model([]));
    const presented = planDimensionPresentation(spanDimensions(model([]))[0]!, anchors);
    expect(presented.support).toBe(NOT_PUBLISHED);
    expect(presented.denominator).toBe(NOT_PUBLISHED);
    expect(presented.censoring).toBe(NOT_PUBLISHED);
    expect(presented.interval).toBe(NOT_PUBLISHED);
    expect(presented.descriptorRevision).toBe(NOT_PUBLISHED);
    expect(presented.anchors).toContain('scope project.tracedecay');
    expect(presented.horizon).toContain('unbounded');
  });
});

function model(metrics: MetricValueV1[]): ObservatoryReadModelV1 {
  return {
    authorized_scope_ref: 'project.tracedecay',
    horizon: { since_micros: 0, until_micros: NOW },
    watermark: 'analytics:4821',
    observed_at_micros: NOW,
    current: true,
    metrics,
    analytics_mode: {
      current: null,
      transition_watermark: null,
      coverage: { eligible: null, observed: 0, completed: 0, censored: 0, unknown: 1, excluded: 0, state: 'unknown' },
      unavailable_reason: 'not_observed',
    },
    comparison: {
      baseline_build: null, candidate_build: null, workload: null, corpus: null,
      environment: null, oracle: null, configuration: null, platform: null,
      rollback_profile: null, eligible_outcomes: null, paired_outcomes: null,
      regression_observed: null, disposition: 'insufficient_evidence',
      coverage: { eligible: null, observed: 0, completed: 0, censored: 0, unknown: 1, excluded: 0, state: 'unknown' },
      unavailable_reason: 'not_observed',
    },
    rejected_arguments: {
      coverage: { eligible: null, observed: 0, completed: 0, censored: 0, unknown: 1, excluded: 0, state: 'unknown' },
      projector_revision: 'observatory-rejected-argument-projector.v1',
      watermark: 'analytics:4821',
      eligible_attempts: null,
      rejected_total: null,
      rejection_rate: null,
      redacted_name_count: 0,
      groups: [],
      unavailable_reason: 'rejected_argument_observations_not_recorded',
    },
  };
}

function metric(name: string, value: number | null): MetricValueV1 {
  return {
    descriptor_revision: 'analytics-feedback.v1',
    metric: name,
    value,
    unit: 'microseconds',
    denominator: 'latency_samples',
    denominator_value: 96,
    coverage: {
      state: value == null ? 'unknown' : 'known',
      eligible: value == null ? null : 96,
      observed: value == null ? 0 : 96,
      completed: value == null ? 0 : 96,
      censored: 0,
      excluded: 0,
      unknown: value == null ? 1 : 0,
    },
    evidence_class: 'measurement',
    provenance: {
      source: 'feedback_observations',
      source_revision: 'feedback-observations.v1',
      projector_revision: 'feedback-system-quality-projector.v1',
      watermark: 'feedback:311',
    },
    cohort: { descriptor_revision: 'latency_samples.v1', eligible_population: 'latency_samples' },
    temporal: {
      horizon: { since_micros: 0, until_micros: NOW },
      baseline_watermark: null,
      delta: null,
    },
    uncertainty: { lower: value, upper: value, reason: null },
    calibration: null,
    unavailable_reason: null,
  };
}
