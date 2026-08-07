import { describe, expect, it } from 'vitest';

import {
  WorkAttemptListV1Schema,
  type WorkAttemptListCoverageV1,
} from '../../contracts/index.ts';
import {
  workAttempt as attempt,
  workAttemptList,
  workRoute as route,
  workTerminal as terminal,
} from '../../test/workAttemptFixture.ts';
import type { WorkResult } from './workApi.ts';
import { workAttemptReading, type WorkAttemptPage } from './workAttemptModel.ts';

/**
 * The attempt-list reading, over pages the generated contract accepts.
 *
 * Every fixture is parsed with `WorkAttemptListV1Schema` before the model sees
 * it. That is the point of the helper rather than an afterthought: a
 * hand-shaped object the daemon could never send would let this file keep
 * passing through a contract change, which is the one failure a binding test
 * exists to catch.
 */

/** A `listed` page, proved against the generated contract before use. */
function page(
  attempts: readonly unknown[],
  coverage?: WorkAttemptListCoverageV1,
): WorkAttemptPage {
  const parsed = WorkAttemptListV1Schema.parse(workAttemptList(attempts, coverage));
  const reading = workAttemptReading({ outcome: 'value', value: parsed });
  if (reading.state !== 'listed') throw new Error(`expected a listed page, got ${reading.state}`);
  return reading.page;
}

describe('the states the attempt list can be in', () => {
  it('reports a read that has not answered as pending, which is not an answer', () => {
    expect(workAttemptReading(undefined)).toEqual({ state: 'pending' });
  });

  it('keeps the daemon typed absence apart from an authorized empty page', () => {
    const absent = workAttemptReading({
      outcome: 'value',
      value: WorkAttemptListV1Schema.parse({ state: 'absent' }),
    });
    expect(absent).toEqual({ state: 'absent' });

    // An authorized page that happens to hold nothing is a different reading:
    // it has a topology and a coverage, and absence has neither.
    const empty = page([]);
    expect(empty.attemptCount).toBe(0);
    expect(empty.topology.generation).toBe('generation-7');
  });

  /**
   * The stale-cursor refusal. A cursor minted under a superseded topology
   * generation is refused by the daemon rather than continued, and the reading
   * has to carry that refusal rather than degrade into an empty page — an empty
   * execution record would say "nothing ran", which is the opposite of "the
   * topology moved while you were paging".
   */
  it('carries a refusal as a refusal, with the state and sentence it arrived with', () => {
    const refusal: WorkResult<never> = {
      outcome: 'refused',
      state: 'conflicting',
      detail: 'the task moved since it was read',
    };
    const reading = workAttemptReading(refusal);

    expect(reading).toEqual({
      state: 'refused',
      chip: 'conflicting',
      detail: 'the task moved since it was read',
    });
  });

  it('says a capped page is partial, so no count off it reads as a total', () => {
    const capped = page([attempt({ taskId: 'root', runId: 'run-1', attemptId: 'attempt-1' })], {
      coverage: 'capped',
      remaining: 41,
      resume: {
        generation: 'generation-7',
        start_after: { attempt_id: 'attempt-1', run_id: 'run-1', task_id: 'root' },
      },
      returned: 1,
    });

    expect(capped.partial).toBe(true);
    expect(page([attempt({ taskId: 'root', runId: 'run-1', attemptId: 'attempt-1' })]).partial).toBe(
      false,
    );
  });
});

describe('executor identity', () => {
  it('attributes an attempt to the route that ran it and counts the diversion', () => {
    const reading = page([
      attempt({ taskId: 'root', runId: 'run-1', attemptId: 'a-1' }),
      attempt({
        taskId: 'middle',
        runId: 'run-1',
        attemptId: 'a-2',
        requested: route('codex', 'route-primary'),
        actual: route('claude', 'route-fallback'),
      }),
    ]);

    expect(reading.executors).toEqual([
      { providerId: 'claude', routeId: 'route-fallback', attempts: 1, diverted: 1, unobserved: 0 },
      { providerId: 'codex', routeId: 'route-primary', attempts: 1, diverted: 0, unobserved: 0 },
    ]);
  });

  /** An attempt whose actual route is unobserved is still attributed to the
   * route that was asked for — and the row says how much of itself is request
   * rather than observation, so the two can never be read as one figure. */
  it('separates observed execution from attribution by request', () => {
    const reading = page([
      attempt({ taskId: 'root', runId: 'run-1', attemptId: 'a-1', actual: null, state: 'running', terminal: null }),
      attempt({ taskId: 'middle', runId: 'run-1', attemptId: 'a-2' }),
    ]);

    expect(reading.executors).toEqual([
      { providerId: 'codex', routeId: 'route-primary', attempts: 2, diverted: 0, unobserved: 1 },
    ]);
  });
});

describe('the retry weave', () => {
  /**
   * The chain is followed, not counted. Three attempts at one task by one run
   * are one lineage of three links in descent order, whatever order the page
   * listed them in.
   */
  it('orders a chain by descent through the recovery source', () => {
    const reading = page([
      attempt({
        taskId: 'root',
        runId: 'run-1',
        attemptId: 'a-3',
        recovery: { reason: 'lease_lost', source_attempt_id: 'a-2', state: 'restarted' },
      }),
      attempt({ taskId: 'root', runId: 'run-1', attemptId: 'a-1', terminal: terminal('failed', 10) }),
      attempt({
        taskId: 'root',
        runId: 'run-1',
        attemptId: 'a-2',
        recovery: { checkpoint: null, source_attempt_id: 'a-1', state: 'resumed' },
        terminal: terminal('failed', 20),
      }),
    ]);

    expect(reading.lineages).toHaveLength(1);
    const lineage = reading.lineages[0];
    expect(lineage?.links.map((link) => link.attemptId)).toEqual(['a-1', 'a-2', 'a-3']);
    expect(lineage?.links.map((link) => link.origin)).toEqual(['fresh', 'resumed', 'restarted']);
    expect(lineage?.restarts).toBe(2);
    expect(lineage?.truncated).toBe(false);
  });

  it('marks a chain whose root is off the page, so its restart count is a floor', () => {
    const reading = page([
      attempt({
        taskId: 'root',
        runId: 'run-1',
        attemptId: 'a-9',
        recovery: { reason: 'process_lost', source_attempt_id: 'a-8', state: 'restarted' },
      }),
    ]);

    expect(reading.lineages[0]?.truncated).toBe(true);
    expect(reading.lineages[0]?.restarts).toBe(0);
  });

  it('calls a chain open when its latest attempt carries no terminal evidence', () => {
    const reading = page([
      attempt({
        taskId: 'root',
        runId: 'run-1',
        attemptId: 'a-1',
        state: 'running',
        terminal: null,
      }),
      attempt({ taskId: 'middle', runId: 'run-1', attemptId: 'b-1' }),
    ]);

    const open = reading.lineages.find((lineage) => lineage.taskId === 'root');
    const closed = reading.lineages.find((lineage) => lineage.taskId === 'middle');
    expect(open?.open).toBe(true);
    expect(closed?.open).toBe(false);
  });

  it('keeps one run per task apart, so two runs at one task are two chains', () => {
    const reading = page([
      attempt({ taskId: 'root', runId: 'run-1', attemptId: 'a-1' }),
      attempt({ taskId: 'root', runId: 'run-2', attemptId: 'b-1' }),
    ]);

    expect(reading.lineages).toHaveLength(2);
    expect(reading.lineages.every((lineage) => lineage.links.length === 1)).toBe(true);
  });
});

describe('the cancellation ladder', () => {
  it('counts the furthest rung each attempt reached', () => {
    const reading = page([
      attempt({
        taskId: 'root',
        runId: 'run-1',
        attemptId: 'a-1',
        state: 'cancellation_requested',
        cancellation: { state: 'requested', value: { request_id: 'c-1', requested_at: 5 } },
        terminal: null,
      }),
      attempt({
        taskId: 'middle',
        runId: 'run-1',
        attemptId: 'a-2',
        state: 'cancellation_escalated',
        cancellation: {
          state: 'escalated',
          value: {
            acknowledgement: {
              acknowledged_at: 12,
              request: { request_id: 'c-2', requested_at: 8 },
            },
            escalated_at: 20,
          },
        },
        terminal: null,
      }),
      attempt({ taskId: 'leaf', runId: 'run-1', attemptId: 'a-3' }),
    ]);

    expect(reading.ladder).toEqual({
      requested: 1,
      acknowledged: 0,
      escalated: 1,
      unrecorded: 0,
    });
  });

  /** An attempt whose state claims a cancellation its record does not carry is
   * a disagreement between two fields of the same row. It is reported, not
   * resolved. */
  it('reports a state that claims a cancellation the record does not carry', () => {
    const reading = page([
      attempt({
        taskId: 'root',
        runId: 'run-1',
        attemptId: 'a-1',
        state: 'cancelled',
        cancellation: { state: 'none' },
        terminal: terminal('cancelled', 30),
      }),
    ]);

    expect(reading.ladder.unrecorded).toBe(1);
  });

  it('does not read an ordinary attempt as a silent cancellation', () => {
    const reading = page([attempt({ taskId: 'root', runId: 'run-1', attemptId: 'a-1' })]);
    expect(reading.ladder).toEqual({
      requested: 0,
      acknowledged: 0,
      escalated: 0,
      unrecorded: 0,
    });
  });
});

describe('the observed terminal order', () => {
  it('orders terminated attempts by the instant they were observed to finish', () => {
    const reading = page([
      attempt({ taskId: 'leaf', runId: 'run-1', attemptId: 'a-3', terminal: terminal('failed', 300) }),
      attempt({ taskId: 'root', runId: 'run-1', attemptId: 'a-1', terminal: terminal('succeeded', 100) }),
      attempt({ taskId: 'middle', runId: 'run-1', attemptId: 'a-2', terminal: terminal('timed_out', 200) }),
    ]);

    expect(reading.terminalOrder.map((observation) => observation.attemptId)).toEqual([
      'a-1',
      'a-2',
      'a-3',
    ]);
    expect(reading.terminalOrder.map((observation) => observation.outcome)).toEqual([
      'succeeded',
      'timed_out',
      'failed',
    ]);
  });

  /** A running attempt has reached no terminal, so it has no instant to be
   * ordered by. Giving it one — now, or the page's last instant — would be this
   * build inventing the measurement the order exists to report. */
  it('leaves an attempt that has not terminated out of the order entirely', () => {
    const reading = page([
      attempt({ taskId: 'root', runId: 'run-1', attemptId: 'a-1', state: 'running', terminal: null }),
      attempt({ taskId: 'middle', runId: 'run-1', attemptId: 'a-2', terminal: terminal('succeeded', 50) }),
    ]);

    expect(reading.terminalOrder.map((observation) => observation.attemptId)).toEqual(['a-2']);
  });
});
