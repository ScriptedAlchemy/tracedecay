import { render } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { LiveActivityPulse } from '../../data/sse/connect.ts';
import type { SseConnectionState } from '../../data/sse/connect.ts';
import { WorkTaskActivity, taskActivityLink, taskActivityReading } from './WorkTaskActivity.tsx';

/**
 * The Work row that reads a real stream.
 *
 * Its whole job is to distinguish three situations a lazier cell would collapse:
 * a live subscription that has received work, a live subscription that has
 * received none, and a subscription that cannot receive anything because the
 * link is down. Only the middle one is a quiet system; reporting the third as
 * the second would be this page claiming there is no work when it simply cannot
 * see.
 */

const link = vi.hoisted(() => ({ state: 'live' as SseConnectionState }));
const feed = vi.hoisted(() => ({ pulses: [] as LiveActivityPulse[] }));

vi.mock('../../data/sse/useEvents.tsx', () => ({
  useEventStreamState: () => ({ state: link.state, lastEventAt: null }),
  useLiveActivity: () => ({ pulses: feed.pulses, revision: feed.pulses.length }),
}));

function pulse(family: string): LiveActivityPulse {
  return { projectId: 'project.alpha', family, streamId: `${family}:project.alpha`, at: 1 };
}

// Module-level fixtures are shared, so each test starts from the same link and
// an empty feed rather than from whatever the previous one left behind.
beforeEach(() => {
  link.state = 'live';
  feed.pulses = [];
});

describe('the Work task-activity reading', () => {
  it('never reports an unreachable stream as a quiet one', () => {
    expect(taskActivityReading('offline', 0)).toContain('unreachable');
    expect(taskActivityReading('offline', 0)).not.toContain('none received');
    expect(taskActivityReading('connecting', 0)).toContain('connecting');
    expect(taskActivityReading('connecting', 0)).not.toContain('none received');
  });

  it('separates a silent live stream from one carrying work', () => {
    expect(taskActivityReading('live', 0)).toBe('subscribed · none in live window');
    expect(taskActivityReading('live', 3)).toBe('subscribed · 3 in live window');
  });

  /**
   * The pulse buffer holds 64 entries across every family, so an unrelated burst
   * evicts task pulses and this figure falls while the stream stays live. Worded
   * as a total it would report an absence of work caused by other work, so the
   * reading has to name the window it measures.
   */
  it('states the count as a window rather than as a total', () => {
    expect(taskActivityReading('live', 0)).not.toContain('received');
    expect(taskActivityReading('live', 7)).toContain('in live window');
  });

  /** Every branch says it is subscribed, because it is: the claim is about this
   * build's own wiring and stays true whatever the link is doing. */
  it('states the subscription in every link state', () => {
    for (const state of ['live', 'connecting', 'offline'] as const) {
      expect(taskActivityReading(state, 0)).toContain('subscribed');
    }
  });

  it('counts only Work task frames, not the other live families', () => {
    link.state = 'live';
    feed.pulses = [pulse('hook_activity'), pulse('task_activity'), pulse('tool_call_activity')];

    const { container } = render(<WorkTaskActivity kind="partial" />);

    // The exact reading, not a substring: `toContain('1 in live window')` is
    // also satisfied by 11 and 21, so it would pass while the filter counted
    // every family.
    expect(container.textContent).toContain('subscribed · 1 in live window');
    expect(container.textContent).not.toContain('3 in live window');
  });

  /**
   * The chip's reading changes with no user action, and the transition from a
   * live stream to an unreachable one is the one this row exists to state. A
   * sighted reader sees it; without a status region nobody else is told.
   */
  it('announces the link state, and does not announce the count', () => {
    link.state = 'offline';
    feed.pulses = [pulse('task_activity')];

    const { container } = render(<WorkTaskActivity kind="partial" />);
    const status = container.querySelector('[role="status"]');

    expect(status?.textContent).toBe('Work task activity: stream unreachable');
    // A polite region that carried the count would read a new number over the
    // user on every accepted frame.
    expect(status?.textContent).not.toMatch(/\d/);
  });

  it('gives each link state its own announcement', () => {
    const announced = (['live', 'connecting', 'offline'] as const).map(taskActivityLink);
    expect(new Set(announced).size).toBe(announced.length);
    expect(taskActivityLink('offline')).toContain('unreachable');
  });

  it('reports the link failing rather than an absence of work', () => {
    link.state = 'offline';
    feed.pulses = [];

    const { container } = render(<WorkTaskActivity kind="partial" />);

    expect(container.textContent).toContain('unreachable');
    expect(container.querySelector('[data-state]')?.getAttribute('data-state')).toBe('partial');
  });
});
