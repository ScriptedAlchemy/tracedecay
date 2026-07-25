import { describe, expect, it } from 'vitest';
import {
  ANALYTICS_EVENT_LIMIT,
  describeWindow,
  familiesSummary,
  familyVerdict,
  formatSpan,
  logFraction,
  percent,
  summarizeDominance,
} from './usage.ts';

/** The exact shape the daemon served on 2026-07-25 for this repository. */
const LIVE_ROWS = [
  { kind: 'skill', category: 'workflow_skill', events: 1 },
  { kind: 'tool', category: 'lcm_session', events: 52 },
  { kind: 'tool', category: 'memory', events: 643 },
  { kind: 'tool', category: 'tracedecay_mcp', events: 6774 },
];

describe('summarizeDominance', () => {
  it('reports the live distribution as dominated by one category', () => {
    const summary = summarizeDominance(LIVE_ROWS);
    expect(summary.total).toBe(7470);
    expect(summary.leader?.category).toBe('tracedecay_mcp');
    expect(percent(summary.leaderShare)).toBe(91);
    expect(summary.spread).toBe(6774);
    expect(summary.dominant).toBe(true);
    expect(summary.rest.map((row) => row.category)).toEqual([
      'memory',
      'lcm_session',
      'workflow_skill',
    ]);
  });

  it('does not claim dominance for an even distribution', () => {
    const summary = summarizeDominance([
      { kind: 'tool', category: 'a', events: 100 },
      { kind: 'tool', category: 'b', events: 90 },
      { kind: 'tool', category: 'c', events: 80 },
    ]);
    expect(summary.dominant).toBe(false);
    expect(percent(summary.leaderShare)).toBe(37);
  });

  it('never claims dominance for a single row, which has nothing to dominate', () => {
    expect(summarizeDominance([{ kind: 'tool', category: 'only', events: 9 }]).dominant).toBe(
      false,
    );
  });

  it('survives an empty payload without inventing a denominator', () => {
    const summary = summarizeDominance([]);
    expect(summary.total).toBe(0);
    expect(summary.leader).toBeNull();
    expect(summary.leaderShare).toBeNull();
    expect(summary.spread).toBeNull();
    expect(summary.dominant).toBe(false);
  });
});

describe('logFraction', () => {
  it('keeps the smallest live row visible against the largest', () => {
    const smallest = logFraction(1, 6774);
    expect(smallest).not.toBeNull();
    // A linear scale would put this row at 0.015% of the band — invisible.
    expect(smallest!).toBeGreaterThan(0.05);
    expect(smallest!).toBeLessThan(0.1);
  });

  it('puts the ceiling at the end of the band and zero at its start', () => {
    expect(logFraction(6774, 6774)).toBe(1);
    expect(logFraction(0, 6774)).toBe(0);
  });

  it('preserves ordering', () => {
    const a = logFraction(52, 6774)!;
    const b = logFraction(643, 6774)!;
    expect(a).toBeLessThan(b);
  });

  it('returns null rather than a fabricated length when there is no ceiling', () => {
    expect(logFraction(5, 0)).toBeNull();
    expect(logFraction(5, Number.NaN)).toBeNull();
  });
});

describe('describeWindow', () => {
  it('recognises the endpoint cap and derives the window span from the rate', () => {
    const window = describeWindow(10_000, 135.36531714965764);
    expect(window.capped).toBe(true);
    expect(window.spanHours).toBeCloseTo(73.87, 1);
    expect(formatSpan(window.spanHours)).toBe('3.1 d');
  });

  it('does not call an under-cap count capped', () => {
    expect(describeWindow(4200, 12).capped).toBe(false);
    expect(describeWindow(ANALYTICS_EVENT_LIMIT - 1, 12).capped).toBe(false);
  });

  it('leaves the span unknown when the rate is missing rather than guessing', () => {
    expect(describeWindow(10_000, null).spanHours).toBeNull();
    expect(describeWindow(10_000, 0).spanHours).toBeNull();
  });

  it('preserves an omitted event count as unknown instead of zero', () => {
    const window = describeWindow(undefined, undefined);
    expect(window.events).toBeNull();
    expect(window.capped).toBe(false);
    expect(window.spanHours).toBeNull();
  });
});

describe('formatSpan', () => {
  it('scales its unit to the magnitude', () => {
    expect(formatSpan(0.5)).toBe('30 min');
    expect(formatSpan(9.4)).toBe('9.4 h');
    expect(formatSpan(30)).toBe('30 h');
    expect(formatSpan(73.87)).toBe('3.1 d');
    expect(formatSpan(600)).toBe('25 d');
  });

  it('renders an em dash for an unknown span', () => {
    expect(formatSpan(null)).toBe('—');
    expect(formatSpan(0)).toBe('—');
  });
});

describe('familyVerdict', () => {
  const live = [
    { family: 'call_graph', missed_events: 0, relevant_events: 0, underused: false, usage_events: 0 },
    {
      family: 'code_context',
      missed_events: -138,
      relevant_events: 0,
      underused: false,
      usage_events: 138,
    },
    {
      family: 'code_search',
      missed_events: -226,
      relevant_events: 0,
      underused: false,
      usage_events: 226,
    },
    {
      family: 'impact_analysis',
      missed_events: 0,
      relevant_events: 0,
      underused: false,
      usage_events: 0,
    },
  ];

  it('separates families with no detector from families that are genuinely covered', () => {
    expect(familyVerdict(live[0]!).state).toBe('unmeasurable');
    expect(familyVerdict(live[3]!).state).toBe('unmeasurable');
    expect(familyVerdict(live[1]!).state).toBe('covered');
    expect(familyVerdict(live[2]!).state).toBe('covered');
  });

  it('reports a genuinely under-used family with its missed count', () => {
    const verdict = familyVerdict({
      family: 'code_search',
      usage_events: 4,
      relevant_events: 30,
      missed_events: 26,
      underused: true,
    });
    expect(verdict.state).toBe('underused');
    expect(verdict.line).toContain('26 moments');
  });

  it('reports an unexercised measured family as idle, not as covered', () => {
    expect(
      familyVerdict({
        family: 'code_search',
        usage_events: 0,
        relevant_events: 0,
        missed_events: 0,
        underused: false,
      }).state,
    ).toBe('idle');
  });

  it('summarises the live payload in one sentence instead of four null rows', () => {
    const summary = familiesSummary(live);
    expect(summary).toContain('2 of 4');
    expect(summary).toContain('zero times');
    expect(summary).toContain('cannot be flagged by construction');
  });

  it('yields the summary to the rows when a family is actually flagged', () => {
    expect(
      familiesSummary([
        { family: 'code_search', usage_events: 1, relevant_events: 9, missed_events: 8, underused: true },
      ]),
    ).toBeNull();
  });

  it('has nothing to say about an empty family list', () => {
    expect(familiesSummary([])).toBeNull();
  });
});
