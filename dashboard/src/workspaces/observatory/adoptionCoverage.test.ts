import { describe, expect, it } from 'vitest';
import type { CoverageStateV1, ObservatoryReadModelV1 } from '../../contracts/generated.ts';
import {
  coverageWindowTruth,
  denominatorFailureTruth,
  eventCoverageReading,
} from './adoptionCoverage.ts';

const NOW_MICROS = 1_753_003_600_000_000;

describe('coverageWindowTruth', () => {
  it('preserves every metric coverage state instead of promoting a current snapshot to Ready', () => {
    const cases: readonly [CoverageStateV1 | undefined, string, string][] = [
      ['known', 'known', 'ready'],
      ['capped', 'capped', 'partial'],
      ['partial', 'partial', 'partial'],
      ['sampled', 'sampled', 'partial'],
      ['stale', 'stale', 'stale'],
      ['unknown', 'unknown', 'unknown'],
      [undefined, 'missing', 'unknown'],
    ];

    for (const [coverage, metricState, presentation] of cases) {
      expect(coverageWindowTruth(readModel(coverage))).toEqual({ metricState, presentation });
    }
  });
});

describe('eligible versus observed coverage binding', () => {
  it('withholds numeric pairs when the metric coverage is not known', () => {
    for (const coverage of ['capped', 'partial', 'sampled', 'stale', 'unknown'] as const) {
      const event = eventCoverageReading(readModel(coverage, 24, 30));
      expect(event.coverage).toBe(coverage);
      expect(event.reading).toBeNull();
      expect(event.integrity.kind).toBe('independent');
    }
  });

  it('keeps a known, independent pair typed instead of deriving a rate', () => {
    const event = eventCoverageReading(readModel('known', 24, 30));
    expect(event.reading).toEqual({ kind: 'measured', observed: 24, eligible: 30 });
  });
});

describe('denominator failure truth', () => {
  it('treats an empty 0-of-0 audit as unknown rather than ready', () => {
    expect(
      denominatorFailureTruth({ failed: 0, total: 0, missing: 0 }),
    ).toMatchObject({ state: 'unknown' });
  });

  it('reports a missing denominator as unknown', () => {
    expect(
      denominatorFailureTruth({ failed: 1, total: 1, missing: 1 }),
    ).toMatchObject({ state: 'unknown' });
  });
});

function readModel(
  coverage: CoverageStateV1 | undefined,
  observed = 24,
  eligible = 24,
): ObservatoryReadModelV1 {
  return {
    authorized_scope_ref: 'project.tracedecay',
    current: true,
    horizon: { since_micros: 0, until_micros: NOW_MICROS },
    metrics:
      coverage === undefined
        ? []
        : [
            {
              calibration: null,
              cohort: {
                descriptor_revision: 'eligible_observability_events.v1',
                eligible_population: 'eligible_observability_events',
              },
              coverage: {
                censored: 0,
                completed: observed,
                eligible,
                excluded: 0,
                observed,
                state: coverage,
                unknown: 0,
              },
              denominator: 'eligible_observability_events',
              denominator_value: eligible,
              descriptor_revision: 'analytics-observability.v1',
              evidence_class: 'measurement',
              metric: 'observability_events',
              provenance: {
                projector_revision: 'observatory-projector.v1',
                source: 'observability_envelope',
                source_revision: 'observability-envelope.v1',
                watermark: 'analytics:4821',
              },
              temporal: {
                baseline_watermark: null,
                delta: null,
                horizon: { since_micros: 0, until_micros: NOW_MICROS },
              },
              unavailable_reason: null,
              uncertainty: { lower: observed, reason: null, upper: observed },
              unit: 'events',
              value: observed,
            },
          ],
    observed_at_micros: NOW_MICROS,
    watermark: 'analytics:4821',
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
