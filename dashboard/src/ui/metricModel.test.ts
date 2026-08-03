import { describe, expect, it } from 'vitest';
import type { MetricValueV1 } from '../contracts/generated.ts';
import {
  availableCount,
  coverageSentence,
  denominatorSentence,
  evidenceQuality,
  groupBySource,
  humanizeMetric,
  metricFigure,
  metricPresentation,
} from './metricModel.ts';

/**
 * The Plan 26 measurement's honesty rules, exercised against the exact shapes
 * `src/application/observability.rs` produces.
 *
 * Two of these are the whole reason the module exists. A metric the projector
 * could not complete arrives as `value: null` with a reason, and must never
 * become a zero. And every known value arrives with a degenerate uncertainty
 * interval (`lower == upper == value`), which must never be dressed up as a
 * measured range.
 */
function metric(overrides: Partial<MetricValueV1> = {}): MetricValueV1 {
  return {
    descriptor_revision: 'analytics-observability.v1',
    metric: 'observability_events',
    value: 128,
    unit: 'events',
    denominator: 'eligible_observability_events',
    denominator_value: 128,
    coverage: {
      state: 'known',
      eligible: 128,
      observed: 128,
      completed: 128,
      censored: 0,
      excluded: 0,
      unknown: 0,
    },
    evidence_class: 'measurement',
    provenance: {
      source: 'observability_envelope',
      source_revision: 'observability-envelope.v1',
      projector_revision: 'observatory-projector.v1',
      watermark: 'analytics:4821',
    },
    cohort: {
      descriptor_revision: 'eligible_observability_events.v1',
      eligible_population: 'eligible_observability_events',
    },
    temporal: {
      horizon: { since_micros: 1_750_000_000_000_000, until_micros: 1_752_592_000_000_000 },
      baseline_watermark: null,
      delta: null,
    },
    uncertainty: { lower: 128, upper: 128, reason: null },
    calibration: null,
    unavailable_reason: null,
    ...overrides,
  };
}

describe('metric figure', () => {
  it('renders an absent measurement as an em dash with no unit', () => {
    const figure = metricFigure(
      metric({ value: null, unavailable_reason: 'no_latency_samples' }),
    );
    expect(figure.value).toBe('—');
    // A unit beside a dash reads as a measurement that was taken and lost.
    expect(figure.unit).toBeNull();
    expect(figure.exact).toBeNull();
  });

  it('never turns an absent measurement into a zero', () => {
    const presentation = metricPresentation(
      metric({ value: null, unavailable_reason: 'accounting_store_unavailable' }),
    );
    expect(presentation.figure).not.toBe('0');
    expect(presentation.available).toBe(false);
    expect(presentation.unavailableReason).toBe('accounting_store_unavailable');
  });

  it('converts microseconds to milliseconds and keeps the server figure', () => {
    const figure = metricFigure(
      metric({ metric: 'feedback_latency_p95', value: 43_250, unit: 'microseconds' }),
    );
    expect(figure.value).toBe('43.25');
    expect(figure.unit).toBe('ms');
    expect(figure.exact).toBe('43,250 µs');
  });

  it('converts a ratio to a percent and keeps the server figure', () => {
    const figure = metricFigure(
      metric({ metric: 'feedback_coverage', value: 0.4237, unit: 'ratio' }),
    );
    expect(figure.value).toBe('42.37');
    expect(figure.unit).toBe('%');
    expect(figure.exact).toBe('0.4237 ratio');
  });

  it('prints an unrecognised unit exactly as the server measured it', () => {
    const figure = metricFigure(metric({ value: 1_284_000, unit: 'tokens' }));
    expect(figure.value).toBe('1,284,000');
    expect(figure.unit).toBe('tokens');
    expect(figure.exact).toBeNull();
  });
});

describe('denominator', () => {
  it('names the eligible population and its size', () => {
    expect(denominatorSentence(metric())).toBe('per eligible observability events · 128');
  });

  it('says the size is unreported rather than printing a bare population', () => {
    expect(denominatorSentence(metric({ denominator_value: null }))).toBe(
      'per eligible observability events · size not reported',
    );
  });
});

describe('coverage', () => {
  it('states observed and completed even when they agree', () => {
    expect(coverageSentence(metric().coverage)).toBe(
      'known · 128 observed, 128 completed · 128 eligible',
    );
  });

  it('keeps censored, excluded, and unknown as separate counts', () => {
    const sentence = coverageSentence({
      state: 'partial',
      eligible: null,
      observed: 90,
      completed: 60,
      censored: 12,
      excluded: 8,
      unknown: 10,
    });
    expect(sentence).toBe(
      'partial · 90 observed, 60 completed, 12 censored, 8 excluded, 10 unknown · eligible population unknown',
    );
  });

  it('never renders a percentage against an unknown eligible population', () => {
    const sentence = coverageSentence({
      state: 'unknown',
      eligible: null,
      observed: 0,
      completed: 0,
      censored: 0,
      excluded: 0,
      unknown: 1,
    });
    expect(sentence).not.toContain('%');
    expect(sentence).toContain('eligible population unknown');
  });
});

describe('uncertainty and calibration', () => {
  it('drops the degenerate interval the composer writes for every known value', () => {
    expect(metricPresentation(metric()).interval).toBeNull();
  });

  it('keeps a real interval', () => {
    const presentation = metricPresentation(
      metric({ uncertainty: { lower: 120, upper: 136, reason: null } }),
    );
    expect(presentation.interval).toBe('120 – 136 events');
  });

  it('reports calibration only when the server attached one', () => {
    expect(metricPresentation(metric()).calibration).toBeNull();
    const calibrated = metricPresentation(
      metric({
        evidence_class: 'calibrated_prediction',
        calibration: {
          estimator_revision: 'estimator.v3',
          calibration_revision: 'calibration.v2',
          cohort_revision: 'cohort.v1',
          support: 4096,
          drift_valid: false,
        },
      }),
    );
    expect(calibrated.calibration).toContain('estimator estimator.v3');
    expect(calibrated.calibration).toContain('support 4,096');
    expect(calibrated.calibration).toContain('drift invalid');
  });

  it('reports a temporal delta only against a wire baseline', () => {
    expect(metricPresentation(metric()).delta).toBeNull();
    const moved = metricPresentation(
      metric({
        temporal: {
          horizon: { since_micros: 0, until_micros: 1 },
          baseline_watermark: 'analytics:4000',
          delta: 12,
        },
      }),
    );
    expect(moved.delta).toBe('+12 events against analytics:4000');
  });
});

describe('evidence class', () => {
  it('maps each class onto its own pattern rather than a severity scale', () => {
    expect(evidenceQuality('measurement')).toBe('measured');
    expect(evidenceQuality('association')).toBe('associated');
    expect(evidenceQuality('calibrated_prediction')).toBe('predicted');
  });
});

describe('grouping', () => {
  it('groups by the producing source and preserves server order inside a group', () => {
    const groups = groupBySource([
      metric({ metric: 'observability_events' }),
      metric({
        metric: 'feedback_latency_p95',
        provenance: {
          source: 'feedback_observations',
          source_revision: 'feedback-observations.v1',
          projector_revision: 'feedback-system-quality-projector.v1',
          watermark: 'feedback:12',
        },
      }),
      metric({ metric: 'observability_failures' }),
    ]);
    expect(groups.map((group) => group.source)).toEqual([
      'observability_envelope',
      'feedback_observations',
    ]);
    expect(groups[0]?.metrics.map((entry) => entry.metric)).toEqual([
      'observability_events',
      'observability_failures',
    ]);
    expect(groups[0]?.label).toBe('observability envelope');
  });

  it('counts only the metrics that actually carry a value', () => {
    expect(availableCount([metric(), metric({ value: null }), metric()])).toBe(2);
  });
});

describe('labels', () => {
  it('spaces the wire identifier without replacing it', () => {
    expect(humanizeMetric('telemetry_drops_lower_bound')).toBe('telemetry drops lower bound');
  });
});
