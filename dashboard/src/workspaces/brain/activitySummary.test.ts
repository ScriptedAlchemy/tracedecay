import { describe, expect, it } from 'vitest';
import type { LiveActivityPulse } from '../../data/sse/connect.ts';
import {
  ageTickIntervalMs,
  familyLabel,
  formatDurationMs,
  summarizeActivity,
  RATE_WINDOW_MS,
} from './activitySummary.ts';

function pulse(family: string, at: number): LiveActivityPulse {
  return { projectId: 'p1', family, streamId: 'code_index', at };
}

const NOW = 1_000_000;

describe('summarizeActivity', () => {
  it('reports nothing at all for an empty ring', () => {
    expect(summarizeActivity([], NOW)).toEqual({
      total: 0,
      families: [],
      peak: null,
      spanMs: null,
      ratePerMinute: 0,
    });
  });

  it('ranks families by count, breaking ties alphabetically', () => {
    const summary = summarizeActivity(
      [
        pulse('heartbeat', NOW - 3_000),
        pulse('code_index_completed', NOW - 2_000),
        pulse('heartbeat', NOW - 1_000),
        pulse('storage_telemetry_invalidated', NOW),
      ],
      NOW,
    );
    expect(summary.families.map((entry) => [entry.family, entry.count])).toEqual([
      ['heartbeat', 2],
      ['code_index_completed', 1],
      ['storage_telemetry_invalidated', 1],
    ]);
    expect(summary.peak).toBe(2);
    expect(summary.total).toBe(4);
    expect(summary.spanMs).toBe(3_000);
  });

  it('measures the rate over a window ending now, so silence falls to zero', () => {
    // A burst of twelve events, then nothing for five minutes. The ring still
    // holds all twelve — it only empties by being overwritten — so a rate
    // measured across the ring's OWN span would still be reporting the burst's
    // original rate long after the stream went quiet. That is the single
    // easiest way for this panel to lie, so it is pinned here.
    const burst = Array.from({ length: 12 }, (_, index) =>
      pulse('heartbeat', NOW - 300_000 + index * 100),
    );
    expect(summarizeActivity(burst, NOW).ratePerMinute).toBe(0);
    // The families are still shown — the ring is real history — but the rate
    // is a statement about the present and is now zero.
    expect(summarizeActivity(burst, NOW).total).toBe(12);
  });

  it('counts only the pulses inside the trailing window', () => {
    const summary = summarizeActivity(
      [
        pulse('heartbeat', NOW - RATE_WINDOW_MS - 1),
        pulse('heartbeat', NOW - RATE_WINDOW_MS + 1),
        pulse('heartbeat', NOW - 1_000),
        pulse('heartbeat', NOW),
      ],
      NOW,
    );
    expect(summary.ratePerMinute).toBe(3);
  });

  it('gives a single pulse no span to divide by', () => {
    expect(summarizeActivity([pulse('heartbeat', NOW)], NOW).spanMs).toBeNull();
  });
});

describe('familyLabel', () => {
  it('names the families the dashboard knows', () => {
    expect(familyLabel('project_registry_changed')).toBe('project registry');
    expect(familyLabel('code_index_completed')).toBe('code index');
  });

  it('keeps an unknown family legible instead of dropping it', () => {
    expect(familyLabel('some_new_thing')).toBe('some new thing');
  });
});

describe('formatDurationMs', () => {
  it('renders an em dash rather than inventing a zero', () => {
    expect(formatDurationMs(null)).toBe('—');
    expect(formatDurationMs(Number.NaN)).toBe('—');
    expect(formatDurationMs(-1)).toBe('—');
  });

  it('coarsens as the reading grows', () => {
    expect(formatDurationMs(400)).toBe('<1s');
    expect(formatDurationMs(4_000)).toBe('4s');
    expect(formatDurationMs(240_000)).toBe('4m');
    expect(formatDurationMs(4 * 3_600_000)).toBe('4h');
  });

  // The name carries the unit because Loom's `tracks.ts` has a same-named
  // seconds formatter. Passing seconds to this one is a 1000x understatement,
  // and these are the readings that would print if a caller mixed them up.
  it('reads a seconds-valued argument as the near-zero it would be in ms', () => {
    expect(formatDurationMs(45)).toBe('<1s');
    expect(formatDurationMs(600)).toBe('<1s');
  });
});

describe('ageTickIntervalMs', () => {
  it('re-reads the clock only as often as the displayed resolution needs', () => {
    expect(ageTickIntervalMs(0)).toBe(1_000);
    expect(ageTickIntervalMs(59_000)).toBe(1_000);
    expect(ageTickIntervalMs(120_000)).toBe(15_000);
    expect(ageTickIntervalMs(7_200_000)).toBe(60_000);
  });
});
