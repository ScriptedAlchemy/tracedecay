import { describe, expect, it } from 'vitest';
import type { ObservatoryReadModelV1 } from '../../contracts/generated.ts';
import {
  COMPARISON_DISPOSITIONS,
  dispositionPresentation,
  performanceComparisonBands,
  resultDimensions,
  subjectDimensions,
} from './performanceComparisons.ts';

describe('dispositionPresentation', () => {
  it('gives insufficient evidence its own state, distinct from reject', () => {
    const insufficient = dispositionPresentation('insufficient_evidence');
    const reject = dispositionPresentation('reject');
    expect(insufficient.state).toBe('unknown');
    expect(reject.state).toBe('denied');
    expect(insufficient.state).not.toBe(reject.state);
    expect(insufficient.label).not.toBe(reject.label);
  });

  it('says in words that insufficient evidence is not a rejection', () => {
    expect(dispositionPresentation('insufficient_evidence').meaning).toContain(
      'not a rejection',
    );
  });

  it('gives all three dispositions distinct labels and states', () => {
    const labels = COMPARISON_DISPOSITIONS.map(
      (disposition) => dispositionPresentation(disposition).label,
    );
    const states = COMPARISON_DISPOSITIONS.map(
      (disposition) => dispositionPresentation(disposition).state,
    );
    expect(new Set(labels).size).toBe(3);
    expect(new Set(states).size).toBe(3);
  });
});

describe('comparison evidence dimensions', () => {
  it('pins baseline and candidate builds as separate requirements', () => {
    const ids = subjectDimensions(model()).map((dimension) => dimension.id);
    expect(ids).toContain('baseline_build');
    expect(ids).toContain('candidate_build');
    expect(ids).toContain('rollback_profile');
  });

  it('keeps per-stratum support, intervals, calibration, flakiness, and deviations separate', () => {
    const ids = resultDimensions(model()).map((dimension) => dimension.id);
    expect(ids).toEqual([
      'outcome_counts',
      'stratum_support',
      'intervals',
      'calibration',
      'risk_coverage',
      'flaky_indeterminate',
      'deviations',
      'paired_outcomes',
    ]);
  });

  it('binds every comparison dimension to the canonical projection', () => {
    for (const band of performanceComparisonBands(model())) {
      for (const dimension of band.dimensions) {
        expect(dimension.reading.kind).toBe('unmeasured');
      }
    }
  });
});

function model(): ObservatoryReadModelV1 {
  return {
    authorized_scope_ref: 'project:test',
    horizon: { since_micros: 0, until_micros: 1 },
    watermark: 'analytics:empty',
    observed_at_micros: 1,
    current: false,
    metrics: [
      ...[
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
      ].map((metric) => ({
        metric,
        value: null,
        coverage: { state: 'unknown' },
        unavailable_reason: 'comparison_evidence_not_recorded',
      })),
    ],
  } as ObservatoryReadModelV1;
}
