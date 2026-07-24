import { describe, expect, it } from 'vitest';
import { familyLabel, formatEventAge, summarizeActivity } from './activitySummary.ts';
import type { LiveActivityPulse } from '../../data/sse/connect.ts';

function pulse(
  family: string,
  at: number,
  projectId: string | null = 'project.alpha',
): LiveActivityPulse {
  return { projectId, family, streamId: family, at };
}

describe('summarizeActivity', () => {
  it('reports an empty ring honestly rather than a zero-filled shape', () => {
    expect(summarizeActivity([])).toEqual({ total: 0, families: [], ratePerMinute: null });
  });

  it('ranks families by count, most active first, ties broken alphabetically', () => {
    const summary = summarizeActivity([
      pulse('heartbeat', 1_000),
      pulse('project_registry_changed', 1_100),
      pulse('heartbeat', 1_200),
      pulse('storage_telemetry_invalidated', 1_300),
      pulse('heartbeat', 1_400),
    ]);
    expect(summary.total).toBe(5);
    expect(summary.families).toEqual([
      { family: 'heartbeat', label: 'heartbeat', count: 3 },
      { family: 'project_registry_changed', label: 'project registry', count: 1 },
      { family: 'storage_telemetry_invalidated', label: 'storage telemetry', count: 1 },
    ]);
  });

  it('leaves the rate unmeasurable for a single pulse rather than fabricating a number', () => {
    const summary = summarizeActivity([pulse('heartbeat', 1_000)]);
    expect(summary.ratePerMinute).toBeNull();
  });

  it('leaves the rate unmeasurable when every pulse landed in the same instant', () => {
    const summary = summarizeActivity([pulse('heartbeat', 1_000), pulse('heartbeat', 1_000)]);
    expect(summary.ratePerMinute).toBeNull();
  });

  it('measures a real rate across the ring\'s own observed span', () => {
    // Two intervals (three pulses) spanning 30s: 2 intervals / 30s * 60s = 4/min.
    const summary = summarizeActivity([
      pulse('heartbeat', 0),
      pulse('heartbeat', 15_000),
      pulse('heartbeat', 30_000),
    ]);
    expect(summary.ratePerMinute).toBeCloseTo(4, 5);
  });
});

describe('familyLabel', () => {
  it('names the known families in plain words', () => {
    expect(familyLabel('heartbeat')).toBe('heartbeat');
    expect(familyLabel('project_registry_changed')).toBe('project registry');
    expect(familyLabel('storage_telemetry_invalidated')).toBe('storage telemetry');
    expect(familyLabel('code_index_updated')).toBe('code index');
  });

  it('de-slugs an unrecognized family instead of hiding it', () => {
    expect(familyLabel('future_signal_kind')).toBe('future signal kind');
  });
});

describe('formatEventAge', () => {
  it('renders an em dash for no observation rather than a fabricated age', () => {
    expect(formatEventAge(null)).toBe('—');
    expect(formatEventAge(-5)).toBe('—');
    expect(formatEventAge(Number.NaN)).toBe('—');
  });

  it('steps through the relative-time vocabulary', () => {
    expect(formatEventAge(500)).toBe('just now');
    expect(formatEventAge(45_000)).toBe('45s ago');
    expect(formatEventAge(120_000)).toBe('2m ago');
    expect(formatEventAge(7_200_000)).toBe('2h ago');
  });
});
