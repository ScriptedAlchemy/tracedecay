import { render } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { LiveActivityPulse } from '../../data/sse/connect.ts';
import type { SseConnectionState } from '../../data/sse/connect.ts';
import { useScope } from '../../data/scope/store.ts';
import {
  WorkTaskActivity,
  taskActivityLink,
  taskActivityReading,
  taskActivityWindow,
} from './WorkTaskActivity.tsx';

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

function pulse(family: string, projectId: string | null = 'project.alpha'): LiveActivityPulse {
  return { projectId, family, streamId: `${family}:${projectId ?? 'none'}`, at: 1 };
}

/** The window the all-projects scope produces, for the readings that are about
 * wording rather than about scoping. */
function counted(observed: number): { observed: number; unattributed: number } {
  return { observed, unattributed: 0 };
}

// Module-level fixtures are shared, so each test starts from the same link and
// an empty feed rather than from whatever the previous one left behind.
beforeEach(() => {
  link.state = 'live';
  feed.pulses = [];
  useScope.getState().selectAllProjects();
});

afterEach(() => {
  useScope.getState().selectAllProjects();
});

describe('the Work task-activity reading', () => {
  it('never reports an unreachable stream as a quiet one', () => {
    expect(taskActivityReading('offline', counted(0))).toContain('unreachable');
    expect(taskActivityReading('offline', counted(0))).not.toContain('none received');
    expect(taskActivityReading('connecting', counted(0))).toContain('connecting');
    expect(taskActivityReading('connecting', counted(0))).not.toContain('none received');
  });

  it('separates a silent live stream from one carrying work', () => {
    expect(taskActivityReading('live', counted(0))).toBe('subscribed · none in live window');
    expect(taskActivityReading('live', counted(3))).toBe('subscribed · 3 in live window');
  });

  /**
   * The pulse buffer holds 64 entries across every family, so an unrelated burst
   * evicts task pulses and this figure falls while the stream stays live. Worded
   * as a total it would report an absence of work caused by other work, so the
   * reading has to name the window it measures.
   */
  it('states the count as a window rather than as a total', () => {
    expect(taskActivityReading('live', counted(0))).not.toContain('received');
    expect(taskActivityReading('live', counted(7))).toContain('in live window');
  });

  /** Every branch says it is subscribed, because it is: the claim is about this
   * build's own wiring and stays true whatever the link is doing. */
  it('states the subscription in every link state', () => {
    for (const state of ['live', 'connecting', 'offline'] as const) {
      expect(taskActivityReading(state, counted(0))).toContain('subscribed');
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

/**
 * Whose task frames this row is allowed to count.
 *
 * `/api/events` is one connection for the whole dashboard and the pulse buffer
 * behind it is shared, so every project's task frames sit in the same 64 entries
 * regardless of which project is selected. Counting the family alone therefore
 * reported project A's work under project B — a false "N in live window" that
 * looks exactly like real work in the scope a reader is actually looking at.
 * There is no per-project event route to switch to, and inventing one is not the
 * fix; the scoping belongs on what the buffer already holds.
 */
describe('the Work task-activity window, by scope', () => {
  const MIXED: LiveActivityPulse[] = [
    pulse('task_activity', 'project.alpha'),
    pulse('task_activity', 'project.beta'),
    pulse('task_activity', 'project.beta'),
    pulse('task_activity', null),
    // Another family, from the selected project, which must not be counted for
    // any scope — the original filter's one correct half.
    pulse('hook_activity', 'project.beta'),
  ];

  it('counts only the selected project’s frames, and never another project’s', () => {
    expect(
      taskActivityWindow(MIXED, {
        kind: 'project',
        projectId: 'project.beta',
        label: 'Beta',
        activation: 'active',
      }),
    ).toEqual({ observed: 2, unattributed: 1 });

    expect(
      taskActivityWindow(MIXED, {
        kind: 'project',
        projectId: 'project.alpha',
        label: 'Alpha',
        activation: 'active',
      }),
    ).toEqual({ observed: 1, unattributed: 1 });
  });

  /** The aggregate answers for every project, so a frame that named none is
   * still a frame it received — and there is nothing to report separately. */
  it('counts every attributed and unattributed task frame under all projects', () => {
    expect(taskActivityWindow(MIXED, { kind: 'all' })).toEqual({
      observed: 4,
      unattributed: 0,
    });
  });

  it('claims nothing for a project the window holds no frames for', () => {
    expect(
      taskActivityWindow(MIXED, {
        kind: 'project',
        projectId: 'project.gamma',
        label: 'Gamma',
        activation: 'active',
      }),
    ).toEqual({ observed: 0, unattributed: 1 });
  });

  /**
   * The rendered reading, which is what a reader actually sees. Asserted as the
   * exact string: `toContain('2 in live window')` is also satisfied by 12 and
   * 32, so it would pass while the row counted every project.
   */
  it('renders the selected project’s count, not the shared buffer’s', () => {
    link.state = 'live';
    feed.pulses = MIXED;
    useScope.getState().selectProject('project.beta', 'Beta', 'active');

    const { container } = render(<WorkTaskActivity kind="partial" />);

    expect(container.textContent).toContain('subscribed · 2 in live window');
    // The four task frames in the buffer, and the three that are not Beta's.
    expect(container.textContent).not.toContain('4 in live window');
    expect(container.textContent).not.toContain('3 in live window');
  });

  /**
   * A frame the daemon sent without an exact scope. It cannot be attributed to
   * the selected project, and it cannot be dropped in silence either: a row that
   * said "none in live window" while task frames were arriving would report an
   * absence of work on the strength of an absence of attribution.
   */
  it('names unattributed frames rather than counting or hiding them', () => {
    link.state = 'live';
    feed.pulses = [pulse('task_activity', null), pulse('task_activity', 'project.alpha')];
    useScope.getState().selectProject('project.beta', 'Beta', 'active');

    const { container } = render(<WorkTaskActivity kind="partial" />);

    expect(container.textContent).toContain(
      'subscribed · none in live window · 1 unattributed',
    );
  });

  /** No unattributed frames, no extra clause: the ordinary reading is unchanged
   * for both scopes. */
  it('says nothing about attribution when every frame carried one', () => {
    expect(
      taskActivityReading('live', { observed: 2, unattributed: 0 }),
    ).toBe('subscribed · 2 in live window');
    expect(taskActivityReading('live', { observed: 2, unattributed: 3 })).toBe(
      'subscribed · 2 in live window · 3 unattributed',
    );
  });
});
