/**
 * The scheduler control's honesty properties.
 *
 * These are mutations against a user's automation state, so the failure modes
 * matter more than the success one: the tests below exist to pin the two ways
 * this surface could lie — reporting a state change that did not happen, and
 * reporting a queue as empty when it could not be read.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';

import { setSchedulerPaused } from './automation.ts';
import {
  AutomationSchedulerStatusV1Schema,
  type AutomationSchedulerStatusV1,
} from '../../contracts/generated.ts';

function status(overrides: Partial<AutomationSchedulerStatusV1> = {}) {
  return {
    status: 'configured',
    paused: false,
    pending_fact_proposals: 2,
    pending_skills: 0,
    pending_review: {
      fact_proposals: { state: 'measured', count: 2, reason: null },
      skills: { state: 'measured', count: 0, reason: null },
    },
    enabled: true,
    scheduler_tick_secs: 300,
    now: 1_700_000_000,
    last_session_activity: 1_699_999_000,
    project_config_path: '/p/.tracedecay/automation.json',
    control_path: '/p/.tracedecay/scheduler-control.json',
    tasks: [],
    ...overrides,
  };
}

function respond(body: unknown, init?: { ok?: boolean; statusCode?: number }) {
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => ({
      ok: init?.ok ?? true,
      status: init?.statusCode ?? 200,
      json: async () => body,
    })),
  );
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('setSchedulerPaused', () => {
  it('POSTs and returns the reading the server took after the change', async () => {
    respond(status({ paused: true, status: 'paused' }));
    const result = await setSchedulerPaused('/api/automation/scheduler/pause');
    expect(result.outcome).toBe('ok');
    if (result.outcome !== 'ok') throw new Error('unreachable');
    expect(result.data.paused).toBe(true);
    expect(result.data.status).toBe('paused');
    const call = vi.mocked(fetch).mock.calls[0];
    expect(call?.[0]).toBe('/api/automation/scheduler/pause');
    expect((call?.[1] as RequestInit | undefined)?.method).toBe('POST');
  });

  it('reports a refused control instead of implying it took effect', async () => {
    respond({}, { ok: false, statusCode: 409 });
    const result = await setSchedulerPaused('/api/automation/scheduler/pause');
    expect(result.outcome).toBe('error');
    // The important negative: no `data` to read, so a caller physically cannot
    // paint the scheduler paused off the back of a rejected request.
    expect('data' in result).toBe(false);
  });

  it('does not accept an acknowledgement in place of a reading', async () => {
    // A handler "simplified" to reply `{"ok":true}` would silently return the
    // dashboard to assuming state. The contract refuses it.
    respond({ ok: true });
    const result = await setSchedulerPaused('/api/automation/scheduler/resume');
    expect(result.outcome).toBe('unsupported_schema');
  });
});

describe('the generated scheduler contract', () => {
  it('keeps an unreadable queue distinguishable from an empty one', () => {
    const unreadable = AutomationSchedulerStatusV1Schema.parse(
      status({
        pending_fact_proposals: null,
        pending_review: {
          fact_proposals: {
            state: 'unreadable',
            count: null,
            reason: 'the project fact authority is not available',
          },
          skills: { state: 'measured', count: 0, reason: null },
        },
      }),
    );
    expect(unreadable.pending_review.fact_proposals.state).toBe('unreadable');
    expect(unreadable.pending_review.skills).toEqual({
      state: 'measured',
      count: 0,
      reason: null,
    });
    // The flat count is null rather than 0 for the unreadable queue: a client
    // reading only the legacy field still cannot mistake it for "none waiting".
    expect(unreadable.pending_fact_proposals).toBeNull();
    expect(unreadable.pending_skills).toBe(0);
  });

  it('rejects an unreadable queue that omits its reason', () => {
    const parsed = AutomationSchedulerStatusV1Schema.safeParse(
      status({
        pending_review: {
          fact_proposals: { state: 'unreadable', count: null },
          skills: { state: 'measured', count: 0, reason: null },
        },
      } as Partial<AutomationSchedulerStatusV1>),
    );
    // `reason` is required on the unreadable arm, so the wire cannot carry an
    // unexplained absence — which would render as a bare em dash with nothing
    // for the operator to act on.
    expect(parsed.success).toBe(false);
  });
});
