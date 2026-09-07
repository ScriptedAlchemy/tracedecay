import { describe, expect, it } from 'vitest';
import {
  ADOPTION_FUNNEL_STAGES,
  DIAGNOSTICS_WINDOW_ROWS,
  NOT_SUCCESS_OUTCOMES,
  RATE_MIN_ELIGIBLE,
  SUPPRESSION_FLOOR,
  eligibleVersusObserved,
  familyRowPresentation,
  familyState,
  funnelConsistency,
  readFamily,
  windowTruth,
  withheldCount,
} from './observedFamilies.ts';

/**
 * The rules under test are the ones a reader is harmed by losing: a withheld
 * cell never becomes a zero, an absence that could not be proved never becomes
 * an absence that was, and a ratio is never taken over a denominator that is
 * missing, impossible, or too small for the plan's own floor.
 */

const COMPLETE = windowTruth('complete', true, 'analytics_events');
const PARTIAL = windowTruth('partial', true, 'analytics_events');

function counts(entries: Record<string, number>) {
  return Object.entries(entries).map(([event_kind, count]) => ({ event_kind, count }));
}

describe('readFamily', () => {
  it('publishes a count at or above the local suppression floor', () => {
    const reading = readFamily(counts({ 'adoption.outcome.linked.v1': 41 }), 'adoption.outcome.linked.v1', COMPLETE);
    expect(reading).toEqual({ kind: 'observed', count: 41 });
  });

  it('withholds a cell below the five-unit floor without printing the count', () => {
    const reading = readFamily(
      counts({ 'adoption.outcome.linked.v1': SUPPRESSION_FLOOR - 1 }),
      'adoption.outcome.linked.v1',
      COMPLETE,
    );
    expect(reading.kind).toBe('suppressed');
    // The number itself must not travel to the surface in any field.
    expect(JSON.stringify(reading)).not.toContain(String(SUPPRESSION_FLOOR - 1));
  });

  it('treats zero rows in a complete window as suppressed, never as a reading of 0', () => {
    const reading = readFamily(counts({ 'other.family.v1': 900 }), 'adoption.outcome.linked.v1', COMPLETE);
    expect(reading.kind).toBe('suppressed');
    if (reading.kind !== 'suppressed') throw new Error('unreachable');
    expect(reading.reason).toContain('complete');
    expect(reading.floor).toBe(SUPPRESSION_FLOOR);
  });

  it('reports an absent family in a partial window as censored by the window, not as silence', () => {
    const reading = readFamily(counts({ 'other.family.v1': 900 }), 'adoption.outcome.linked.v1', PARTIAL);
    expect(reading.kind).toBe('censored');
    if (reading.kind !== 'censored') throw new Error('unreachable');
    expect(reading.reason).toContain('partial');
    expect(reading.reason).toContain(DIAGNOSTICS_WINDOW_ROWS.toLocaleString());
  });

  it('never counts a store that did not answer', () => {
    const reading = readFamily(
      counts({ 'adoption.outcome.linked.v1': 900 }),
      'adoption.outcome.linked.v1',
      windowTruth('unknown', false, 'none'),
    );
    expect(reading.kind).toBe('unreadable');
    if (reading.kind !== 'unreadable') throw new Error('unreachable');
    expect(reading.reason).toContain('none');
  });

  it('gives suppression and window censoring different states, so neither reads as the other', () => {
    expect(familyState({ kind: 'suppressed', floor: 5, reason: 'x' })).toBe('redacted');
    expect(familyState({ kind: 'censored', reason: 'x' })).toBe('partial');
    expect(familyState({ kind: 'unreadable', reason: 'x' })).toBe('unavailable');
    expect(familyState({ kind: 'observed', count: 9 })).toBe('ready');
  });
});

describe('familyRowPresentation', () => {
  it('prints an em dash and the reason, never a zero, for every unpublishable reading', () => {
    for (const reading of [
      { kind: 'suppressed', floor: 5, reason: 'below the floor' },
      { kind: 'censored', reason: 'window could not prove it' },
      { kind: 'unreadable', reason: 'no store' },
    ] as const) {
      const row = familyRowPresentation('x.v1', 'x', reading);
      expect(row.available).toBe(false);
      expect(row.figure).toBe('—');
      expect(row.figure).not.toBe('0');
      expect(row.reason).toBe(reading.reason);
      expect(row.denominator).toBe('not published');
    }
  });

  it('counts withheld cells so a ledger of dashes can say what kind of absence it is', () => {
    const rows = [
      familyRowPresentation('a.v1', 'a', { kind: 'observed', count: 11 }),
      familyRowPresentation('b.v1', 'b', { kind: 'suppressed', floor: 5, reason: 'r' }),
      familyRowPresentation('c.v1', 'c', { kind: 'censored', reason: 'r' }),
    ];
    expect(withheldCount(rows)).toBe(2);
  });
});

describe('eligibleVersusObserved', () => {
  it('withholds the remainder when the denominator is missing', () => {
    const reading = eligibleVersusObserved(120, null);
    expect(reading.kind).toBe('denominator_missing');
    if (reading.kind !== 'denominator_missing') throw new Error('unreachable');
    expect(reading.reason).toContain('withheld');
  });

  it('withholds the remainder when the observed count is missing', () => {
    expect(eligibleVersusObserved(null, 400).kind).toBe('observed_missing');
  });

  it('reports an impossible pair as a contradiction rather than clamping it', () => {
    const reading = eligibleVersusObserved(9, 4);
    expect(reading.kind).toBe('contradiction');
    if (reading.kind !== 'contradiction') throw new Error('unreachable');
    expect(reading.reason).toContain('impossible');
    expect(reading.reason).not.toContain('0 ');
  });

  it('retains a pair below the rate floor because the dashboard derives no rate', () => {
    const reading = eligibleVersusObserved(4, RATE_MIN_ELIGIBLE - 1);
    expect(reading).toEqual({
      kind: 'measured',
      observed: 4,
      eligible: RATE_MIN_ELIGIBLE - 1,
    });
  });

  it('keeps an independently published count pair without deriving a dashboard ratio or remainder', () => {
    const reading = eligibleVersusObserved(30, 40);
    expect(reading).toEqual({ kind: 'measured', observed: 30, eligible: 40 });
  });
});

describe('funnelConsistency', () => {
  it('is the plan funnel, in the plan order', () => {
    expect([...ADOPTION_FUNNEL_STAGES]).toEqual([
      'Eligible',
      'Enabled',
      'Available',
      'Invoked',
      'Terminal',
      'IndependentlyUseful',
      'RepeatUseful',
    ]);
  });

  it('claims nothing when fewer than two stages carry a count', () => {
    const reading = funnelConsistency(ADOPTION_FUNNEL_STAGES.map((stage) => ({ stage, count: null })));
    expect(reading.kind).toBe('not_evaluable');
    if (reading.kind !== 'not_evaluable') throw new Error('unreachable');
    expect(reading.measured).toBe(0);
  });

  it('accepts a monotone chain and skips unmeasured stages rather than reading them as zero', () => {
    const reading = funnelConsistency([
      { stage: 'Eligible', count: 100 },
      { stage: 'Enabled', count: null },
      { stage: 'Available', count: 60 },
      { stage: 'Invoked', count: 60 },
    ]);
    expect(reading).toEqual({ kind: 'consistent', measured: 3 });
  });

  it('reports a rising stage as a contradiction rather than drawing it shorter', () => {
    const reading = funnelConsistency([
      { stage: 'Invoked', count: 10 },
      { stage: 'Terminal', count: 12 },
    ]);
    expect(reading.kind).toBe('contradiction');
    if (reading.kind !== 'contradiction') throw new Error('unreachable');
    expect(reading.earlier).toBe('Invoked');
    expect(reading.later).toBe('Terminal');
  });
});

describe('NOT_SUCCESS_OUTCOMES', () => {
  it('carries the nine signals Plan 26 refuses as success outcomes', () => {
    expect([...NOT_SUCCESS_OUTCOMES]).toEqual([
      'display',
      'click',
      'invocation',
      'process completion',
      'self-report',
      'cards closed',
      'tests run',
      'token volume',
      'subjective trust',
    ]);
  });
});
