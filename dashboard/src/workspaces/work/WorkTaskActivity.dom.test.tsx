import { render } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import type { LiveActivityPulse } from '../../data/sse/connect.ts';
import type { SseConnectionState } from '../../data/sse/connect.ts';
import { WorkTaskActivity, taskActivityReading } from './WorkTaskActivity.tsx';

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

describe('the Work task-activity reading', () => {
  it('never reports an unreachable stream as a quiet one', () => {
    expect(taskActivityReading('offline', 0)).toContain('unreachable');
    expect(taskActivityReading('offline', 0)).not.toContain('none received');
    expect(taskActivityReading('connecting', 0)).toContain('connecting');
    expect(taskActivityReading('connecting', 0)).not.toContain('none received');
  });

  it('separates a silent live stream from one carrying work', () => {
    expect(taskActivityReading('live', 0)).toBe('subscribed · none received');
    expect(taskActivityReading('live', 3)).toBe('subscribed · 3 received');
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

    expect(container.textContent).toContain('1 received');
  });

  it('reports the link failing rather than an absence of work', () => {
    link.state = 'offline';
    feed.pulses = [];

    const { container } = render(<WorkTaskActivity kind="partial" />);

    expect(container.textContent).toContain('unreachable');
    expect(container.querySelector('[data-state]')?.getAttribute('data-state')).toBe('partial');
  });
});
