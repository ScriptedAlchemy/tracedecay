import { describe, expect, it } from 'vitest';
import {
  ageTickIntervalMs,
  familyLabel,
  formatDuration,
  summarizeActivity,
} from './activitySummary.ts';
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
    expect(summarizeActivity([], 60_000)).toEqual({
      total: 0,
      families: [],
      peak: null,
      spanMs: null,
      ratePerMinute: 0,
    });
  });

  it('ranks families by count, most active first, ties broken alphabetically', () => {
    const summary = summarizeActivity(
      [
        pulse('heartbeat', 1_000),
        pulse('project_registry_changed', 1_100),
        pulse('heartbeat', 1_200),
        pulse('storage_telemetry_invalidated', 1_300),
        pulse('heartbeat', 1_400),
      ],
      2_000,
    );
    expect(summary.total).toBe(5);
    expect(summary.families).toEqual([
      { family: 'heartbeat', label: 'heartbeat', count: 3 },
      { family: 'project_registry_changed', label: 'project registry', count: 1 },
      { family: 'storage_telemetry_invalidated', label: 'storage telemetry', count: 1 },
    ]);
  });

  it('measures a single recent pulse against the trailing window', () => {
    const summary = summarizeActivity([pulse('heartbeat', 1_000)], 2_000);
    expect(summary.ratePerMinute).toBe(1);
    expect(summary.spanMs).toBeNull();
  });

  it('drops old pulses from the trailing rate without deleting ring history', () => {
    const summary = summarizeActivity(
      [pulse('heartbeat', 1_000), pulse('heartbeat', 61_000)],
      62_000,
    );
    expect(summary.total).toBe(2);
    expect(summary.spanMs).toBe(60_000);
    expect(summary.ratePerMinute).toBe(1);
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

describe('formatDuration', () => {
  it('renders an em dash for no observation rather than a fabricated age', () => {
    expect(formatDuration(null)).toBe('—');
    expect(formatDuration(-5)).toBe('—');
    expect(formatDuration(Number.NaN)).toBe('—');
  });

  it('steps through the relative-time vocabulary', () => {
    expect(formatDuration(500)).toBe('<1s');
    expect(formatDuration(45_000)).toBe('45s');
    expect(formatDuration(120_000)).toBe('2m');
    expect(formatDuration(7_200_000)).toBe('2h');
  });
});

describe('ageTickIntervalMs', () => {
  it('slows the clock as the displayed age loses resolution', () => {
    expect(ageTickIntervalMs(5_000)).toBe(1_000);
    expect(ageTickIntervalMs(120_000)).toBe(15_000);
    expect(ageTickIntervalMs(7_200_000)).toBe(60_000);
  });
});
