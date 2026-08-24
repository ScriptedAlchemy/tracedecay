import { describe, expect, it } from 'vitest';
import type { MetricValueV1 } from '../../contracts/generated.ts';
import {
  NOT_PUBLISHED,
  NO_FIGURE,
  anchorSentence,
  censoringSentence,
  dimensionState,
  horizonSentence,
  intervalSentence,
  measuredCount,
  planDimensionPresentation,
  readMetric,
  supportSentence,
  type PlanDimension,
  type ReadAnchors,
} from './planDimension.ts';

/**
 * The reading rule the three Plan 26 accounting views share.
 *
 * The assertions are almost entirely about what must NOT happen: an absent
 * measurement must never become 0, an unpublished dimension must never borrow a
 * denominator it does not have, and an unpublished dimension must stay
 * distinguishable from one the daemon looked for and could not answer.
 */

const NOW = 1_753_003_600_000_000;

const ANCHORS: ReadAnchors = {
  authorizedScopeRef: 'project.tracedecay',
  watermark: 'analytics:4821',
  horizon: { since_micros: 0, until_micros: NOW },
};

describe('readMetric', () => {
  it('reports a metric the projector does not emit as unpublished', () => {
    const reading = readMetric([], 'latency_p50', 'no read route projects percentiles');
    expect(reading.kind).toBe('unpublished');
    expect(reading.kind === 'unpublished' && reading.reason).toBe(
      'no read route projects percentiles',
    );
  });

  it('reports a metric emitted without a value as unmeasured, carrying the wire reason', () => {
    const reading = readMetric(
      [{ ...metric('latency_p95', null), unavailable_reason: 'no_latency_samples' }],
      'latency_p95',
      'unused',
    );
    expect(reading.kind).toBe('unmeasured');
    expect(reading.kind === 'unmeasured' && reading.reason).toBe('no_latency_samples');
  });

  it('says the projector published no reason rather than inventing one', () => {
    const reading = readMetric([metric('latency_p95', null)], 'latency_p95', 'unused');
    expect(reading.kind === 'unmeasured' && reading.reason).toBe(
      'the projector published no reason',
    );
  });

  it('treats a measured zero as a measurement, not as an absence', () => {
    // The distinction the whole module exists for: 0 observed events is a
    // reading; a missing value is not.
    const reading = readMetric([metric('telemetry_drops', 0)], 'telemetry_drops', 'unused');
    expect(reading.kind).toBe('measured');
    const presented = planDimensionPresentation(
      { id: 'telemetry_drops', label: 'drops', requirement: 'r', reading },
      ANCHORS,
    );
    expect(presented.available).toBe(true);
    expect(presented.figure).toBe('0');
    expect(presented.reason).toBeNull();
  });
});

describe('dimensionState', () => {
  it('keeps unpublished and unmeasured on different chips', () => {
    expect(dimensionState({ kind: 'unpublished', reason: 'r' })).toBe('unsupported');
    expect(dimensionState({ kind: 'unmeasured', metric: metric('m', null), reason: 'r' })).toBe(
      'unknown',
    );
    expect(dimensionState({ kind: 'measured', metric: metric('m', 1) })).toBe('ready');
  });
});

describe('planDimensionPresentation', () => {
  it('renders an unpublished dimension as an em dash and never as zero', () => {
    const presented = planDimensionPresentation(dimension('unpublished'), ANCHORS);
    expect(presented.figure).toBe(NO_FIGURE);
    expect(presented.figure).not.toBe('0');
    expect(presented.available).toBe(false);
    expect(presented.unit).toBeNull();
  });

  it('renders an unmeasured dimension as an em dash and never as zero', () => {
    const presented = planDimensionPresentation(dimension('unmeasured'), ANCHORS);
    expect(presented.figure).toBe(NO_FIGURE);
    expect(presented.figure).not.toBe('0');
    expect(presented.available).toBe(false);
  });

  it('states support, denominator, censoring, interval, and revision as not published for an unpublished dimension', () => {
    const presented = planDimensionPresentation(dimension('unpublished'), ANCHORS);
    // Every field Plan 26 requires on every card is present and answers
    // honestly, rather than being omitted as though it were an oversight.
    expect(presented.support).toBe(NOT_PUBLISHED);
    expect(presented.denominator).toBe(NOT_PUBLISHED);
    expect(presented.censoring).toBe(NOT_PUBLISHED);
    expect(presented.interval).toBe(NOT_PUBLISHED);
    expect(presented.descriptorRevision).toBe(NOT_PUBLISHED);
  });

  it('keeps the wire denominator, coverage, and descriptor revision for an unmeasured dimension', () => {
    // The daemon published this metric and could not value it. The frame it
    // did publish is real and stays visible.
    const presented = planDimensionPresentation(dimension('unmeasured'), ANCHORS);
    expect(presented.support).toBe('7 observed');
    expect(presented.denominator).toContain('latency samples');
    expect(presented.descriptorRevision).toBe('analytics-observability.v1');
    expect(presented.censoring).toContain('censored');
  });

  it('falls back to the read anchors only when the dimension has no metric of its own', () => {
    expect(planDimensionPresentation(dimension('unpublished'), ANCHORS).anchors).toBe(
      'scope project.tracedecay · watermark analytics:4821',
    );
    expect(planDimensionPresentation(dimension('measured'), ANCHORS).anchors).toBe(
      'scope project.tracedecay · watermark feedback:311',
    );
  });
});

describe('supportSentence and censoringSentence', () => {
  it('states support as an observed count, not as an eligible one', () => {
    expect(supportSentence({ kind: 'measured', metric: metric('m', 4) })).toBe('7 observed');
  });

  it('keeps censored, excluded, and unknown as three separate counts', () => {
    const reading = {
      kind: 'measured' as const,
      metric: {
        ...metric('m', 4),
        coverage: {
          state: 'partial' as const,
          eligible: 12,
          observed: 7,
          completed: 4,
          censored: 2,
          excluded: 1,
          unknown: 3,
        },
      },
    };
    expect(censoringSentence(reading)).toBe('2 censored · 1 excluded · 3 unknown');
  });
});

describe('intervalSentence', () => {
  it('refuses to print a degenerate bound as a measured interval', () => {
    // The composer fills lower/upper with the point value for every known
    // value; that is a placeholder, not a bound.
    expect(intervalSentence({ kind: 'measured', metric: metric('m', 4) })).toBe(
      'no measured interval',
    );
  });

  it('prints a genuine interval with its unit', () => {
    const reading = {
      kind: 'measured' as const,
      metric: { ...metric('m', 4), uncertainty: { lower: 2, upper: 9, reason: null } },
    };
    expect(intervalSentence(reading)).toBe('2 – 9 microseconds');
  });

  it('prints the projector reason when there are no bounds at all', () => {
    const reading = {
      kind: 'unmeasured' as const,
      metric: {
        ...metric('m', null),
        uncertainty: { lower: null, upper: null, reason: 'no_latency_samples' },
      },
      reason: 'no_latency_samples',
    };
    expect(intervalSentence(reading)).toBe('no_latency_samples');
  });
});

describe('horizonSentence', () => {
  it('reads an open-ended window as unbounded rather than as 1970', () => {
    expect(horizonSentence({ since_micros: 0, until_micros: NOW })).toContain('unbounded');
  });
});

describe('anchorSentence', () => {
  it('anchors on scope and watermark only', () => {
    const sentence = anchorSentence({ kind: 'unpublished', reason: 'r' }, ANCHORS);
    // Safe anchors: no path, no query, no payload label.
    expect(sentence).toBe('scope project.tracedecay · watermark analytics:4821');
    expect(sentence).not.toContain('/');
  });
});

describe('measuredCount', () => {
  it('counts only the dimensions that carry a figure', () => {
    expect(
      measuredCount([dimension('measured'), dimension('unmeasured'), dimension('unpublished')]),
    ).toBe(1);
  });
});

function dimension(kind: 'measured' | 'unmeasured' | 'unpublished'): PlanDimension {
  const base = { id: 'latency_p95', label: 'latency p95', requirement: 'p95 with support' };
  if (kind === 'unpublished') {
    return { ...base, reading: { kind: 'unpublished', reason: 'no read route projects this' } };
  }
  if (kind === 'unmeasured') {
    return {
      ...base,
      reading: { kind: 'unmeasured', metric: metric('latency_p95', null), reason: 'no_samples' },
    };
  }
  return { ...base, reading: { kind: 'measured', metric: metric('latency_p95', 43_250) } };
}

function metric(name: string, value: number | null): MetricValueV1 {
  return {
    descriptor_revision: 'analytics-observability.v1',
    metric: name,
    value,
    unit: 'microseconds',
    denominator: 'latency_samples',
    denominator_value: 12,
    coverage: {
      state: 'partial',
      eligible: 12,
      observed: 7,
      completed: 4,
      censored: 1,
      excluded: 0,
      unknown: 2,
    },
    evidence_class: 'measurement',
    provenance: {
      source: 'feedback_observations',
      source_revision: 'feedback-observations.v1',
      projector_revision: 'feedback-system-quality-projector.v1',
      watermark: 'feedback:311',
    },
    cohort: {
      descriptor_revision: 'latency_samples.v1',
      eligible_population: 'latency_samples',
    },
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
