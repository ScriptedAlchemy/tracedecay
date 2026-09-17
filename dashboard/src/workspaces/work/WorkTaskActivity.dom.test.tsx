import { render } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { LiveActivityPulse } from '../../data/sse/connect.ts';
import type { SseConnectionState } from '../../data/sse/connect.ts';
import { useScope } from '../../data/scope/store.ts';
import {
  WorkActivityLedger,
  WorkTaskActivity,
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
  return { projectId, family, streamId: `${family}:${projectId ?? 'none'}`, at: 1,
    eventId: `fixture:${family}:${projectId ?? 'none'}`, observationTime: '1000' };
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
});

describe('the live task activity ledger', () => {
  it('lists task frames newest first with the detail word each frame carried', () => {
    link.state = 'live';
    feed.pulses = [
      { ...pulse('task_activity'), eventId: 'run:task:1', observationTime: '1800000000000000', detail: null },
      { ...pulse('hook_activity'), eventId: 'run:hook:1', observationTime: '1800000001000000', detail: 'file_edit' },
      { ...pulse('task_activity'), eventId: 'run:task:2', observationTime: '1800000002000000', detail: 'leased' },
    ];

    const { container } = render(<WorkActivityLedger />);

    const rows = container.querySelectorAll('[data-work-activity-row]');
    expect(rows).toHaveLength(2);
    expect(rows[0]?.textContent).toContain('run:task:2');
    expect(rows[0]?.textContent).toContain('leased');
    expect(rows[0]?.textContent).toContain('2027-01-15T08:00:02.000Z');
    // A frame that named no detail says so rather than inventing a kind, and
    // no row claims a task identity the stream never carried.
    expect(rows[1]?.textContent).toContain('kind not carried by the frame');
    expect(container.querySelector('caption')?.textContent).toContain('no task identity');
    expect(container.textContent).not.toContain('file_edit');
  });

  it('marks an unattributed frame under a selected project instead of claiming or dropping it', () => {
    link.state = 'live';
    useScope.getState().selectProject('project.alpha', 'Alpha', 'selected');
    feed.pulses = [
      { ...pulse('task_activity', null), eventId: 'run:task:none', detail: null },
      { ...pulse('task_activity', 'project.beta'), eventId: 'run:task:beta', detail: null },
      { ...pulse('task_activity', 'project.alpha'), eventId: 'run:task:alpha', detail: 'running' },
    ];

    const { container } = render(<WorkActivityLedger />);

    expect(container.querySelectorAll('[data-work-activity-row="scoped"]')).toHaveLength(1);
    expect(container.querySelectorAll('[data-work-activity-row="unattributed"]')).toHaveLength(1);
    expect(container.textContent).not.toContain('run:task:beta');
    expect(container.textContent).toContain('1 unattributed');
  });

  it('never draws an unreachable stream as a quiet ledger', () => {
    link.state = 'offline';
    feed.pulses = [];

    const { container } = render(<WorkActivityLedger />);

    const empty = container.querySelector('[data-work-activity-empty]');
    expect(empty?.getAttribute('data-work-activity-empty')).toBe('offline');
    expect(empty?.textContent).toContain('unreachable');
    expect(empty?.textContent).not.toContain('No task frame');
  });
});
