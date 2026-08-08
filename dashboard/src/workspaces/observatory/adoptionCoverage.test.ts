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
      denominatorFailureTruth({ failed: 0, total: 0, missing: 0, selfReferential: 0 }),
    ).toMatchObject({ state: 'unknown' });
  });

  it('reports a self-referential denominator as a conflict, not unsupported', () => {
    expect(
      denominatorFailureTruth({ failed: 1, total: 1, missing: 0, selfReferential: 1 }),
    ).toMatchObject({ state: 'conflicting' });
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
  };
}
