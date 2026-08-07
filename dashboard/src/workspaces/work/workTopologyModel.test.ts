import { describe, expect, it } from 'vitest';

import { WorkAttemptListV1Schema, type WorkAttemptListV1 } from '../../contracts/index.ts';
import {
  workAttempt as attempt,
  workAttemptList,
  workRoute as route,
  workTerminal as terminal,
} from '../../test/workAttemptFixture.ts';
import type { WorkResult } from './workApi.ts';
import {
  WORK_TOPOLOGY_DIMENSIONS,
  executorKeyOf,
  workTopologyReading,
} from './workTopologyModel.ts';

/**
 * The execution-topology reading, over pages the generated contract accepts.
 *
 * Every fixture is parsed with `WorkAttemptListV1Schema` before the model sees
 * it, for the same reason the attempt-model tests do: a hand-shaped object the
 * daemon could never send would let this file keep passing through a contract
 * change.
 */

function listed(attempts: readonly unknown[]): WorkResult<WorkAttemptListV1> {
  return {
    outcome: 'value',
    value: WorkAttemptListV1Schema.parse(workAttemptList(attempts)),
  };
}

describe('the states the lens can be in', () => {
  it('reports an unissued read as pending on every derived channel', () => {
    const reading = workTopologyReading(undefined);
    expect(reading.attempts.state).toBe('pending');
    expect(reading.binding.available).toBe(false);
    expect(reading.threads.available).toBe(false);
    expect(reading.worktreeLanes.available).toBe(false);
    if (reading.threads.available) throw new Error('unreachable');
    expect(reading.threads.state).toBe('loading');
  });

  it('carries a refusal in the daemon taxonomy rather than an empty weave', () => {
    const reading = workTopologyReading({
      outcome: 'refused',
      state: 'unavailable',
      detail: 'the Work runtime is unavailable',
    });
    expect(reading.attempts.state).toBe('refused');
    if (reading.threads.available) throw new Error('expected an absent channel');
    expect(reading.threads.state).toBe('unavailable');
    expect(reading.threads.detail).toContain('refused');
  });

  it('keeps the typed daemon absence apart from an authorized empty page', () => {
    const absent = workTopologyReading({
      outcome: 'value',
      value: WorkAttemptListV1Schema.parse({ state: 'absent' }),
    });
    expect(absent.attempts.state).toBe('absent');
    if (absent.threads.available) throw new Error('expected an absent channel');
    expect(absent.threads.state).toBe('denied');
    expect(absent.binding.available).toBe(false);

    // An authorized page holding nothing still pins its topology generation.
    const empty = workTopologyReading(listed([]));
    expect(empty.attempts.state).toBe('listed');
    expect(empty.binding.available).toBe(true);
    if (!empty.binding.available) throw new Error('unreachable');
    expect(empty.binding.value.generation).toBe('generation-7');
    // No attempt means no placement was proved; the channel says why.
    if (empty.threads.available) throw new Error('expected an absent channel');
    expect(empty.threads.state).toBe('complete_zero_findings');
  });
});

describe('the executor weave', () => {
  it('threads by the route that actually ran, counting divergence and repeated crossings', () => {
    const reading = workTopologyReading(
      listed([
        attempt({ taskId: 'alpha', runId: 'run-1', attemptId: 'a-1' }),
        // A retry on the same task by the same executor: a repeated crossing.
        attempt({
          taskId: 'alpha',
          runId: 'run-1',
          attemptId: 'a-2',
          recovery: { reason: 'lease_lost', source_attempt_id: 'a-1', state: 'restarted' },
        }),
        // Requested on the primary, ran on the fallback: the fallback thread
        // carries it and counts it as diverted.
        attempt({
          taskId: 'beta',
          runId: 'run-2',
          attemptId: 'b-1',
          actual: route('claude', 'route-fallback'),
        }),
        // Requested but never observed to run: stays on the requested thread,
        // counted as unobserved, and its landing is open.
        attempt({
          taskId: 'gamma',
          runId: 'run-3',
          attemptId: 'c-1',
          actual: null,
          state: 'running',
          terminal: null,
        }),
      ]),
    );

    if (!reading.threads.available) throw new Error('expected threads');
    const threads = reading.threads.value;
    expect(threads.map((thread) => thread.executorKey)).toEqual([
      executorKeyOf('codex', 'route-primary'),
      executorKeyOf('claude', 'route-fallback'),
    ]);

    const primary = threads[0]!;
    expect(primary.attempts).toBe(3);
    expect(primary.diverted).toBe(0);
    expect(primary.unobserved).toBe(1);
    expect(primary.landings.map((landing) => landing.taskId)).toEqual(['alpha', 'gamma']);
    expect(primary.landings[0]).toMatchObject({ crossings: 2, terminal: true, open: false });
    expect(primary.landings[1]).toMatchObject({ crossings: 1, terminal: false, open: true });

    const fallback = threads[1]!;
    expect(fallback.attempts).toBe(1);
    expect(fallback.diverted).toBe(1);
    expect(fallback.backends).toEqual(['codex_cli']);
    expect(fallback.models).toEqual(['model-1']);
  });
});

describe('the worktree lanes', () => {
  it('pins each lane to the exact repository, ref, and commit identities it was read with', () => {
    const reading = workTopologyReading(
      listed([
        attempt({
          taskId: 'alpha',
          runId: 'run-1',
          attemptId: 'a-1',
          placement: { commit: 'commit-alpha', reference: 'refs/heads/main' },
        }),
        attempt({
          taskId: 'beta',
          runId: 'run-2',
          attemptId: 'b-1',
          actual: route('claude', 'route-fallback'),
          placement: {
            worktreeId: 'worktree-lane',
            worktreeRoot: '/w/lane',
            repositoryId: 'repository-2',
            commit: 'commit-beta',
          },
        }),
        attempt({
          taskId: 'gamma',
          runId: 'run-3',
          attemptId: 'c-1',
          terminal: terminal('failed', 50),
          placement: {
            worktreeId: 'worktree-lane',
            worktreeRoot: '/w/lane',
            repositoryId: 'repository-2',
            commit: 'commit-beta',
          },
        }),
      ]),
    );

    if (!reading.worktreeLanes.available) throw new Error('expected lanes');
    const lanes = reading.worktreeLanes.value;
    expect(lanes.map((lane) => lane.worktreeId)).toEqual(['worktree-lane', 'worktree']);

    const lane = lanes[0]!;
    expect(lane.attempts).toBe(2);
    expect(lane.worktreeRoot).toBe('/w/lane');
    expect(lane.repositoryIds).toEqual(['repository-2']);
    // Two attempts at one ref pin collapse to one pinned identity.
    expect(lane.refs).toEqual([{ reference: null, commit: 'commit-beta' }]);
    expect(lane.taskIds).toEqual(['beta', 'gamma']);
    expect(lane.executorKeys).toEqual([
      executorKeyOf('claude', 'route-fallback'),
      executorKeyOf('codex', 'route-primary'),
    ]);

    const main = lanes[1]!;
    expect(main.refs).toEqual([{ reference: 'refs/heads/main', commit: 'commit-alpha' }]);
  });
});

describe('the dimensions no contract carries', () => {
  it('states the three missing lane families as schema absences that never disappear', () => {
    // On a healthy page as much as on a refused one: the lane families are
    // catalog facts, not read outcomes.
    const reading = workTopologyReading(
      listed([attempt({ taskId: 'alpha', runId: 'run-1', attemptId: 'a-1' })]),
    );
    for (const channel of [
      reading.branchTopology,
      reading.reviewTopology,
      reading.integrationStrategy,
    ]) {
      expect(channel.available).toBe(false);
      if (channel.available) throw new Error('unreachable');
      expect(channel.state).toBe('unsupported_schema');
      expect(channel.detail).toContain('ExecutionTopologyViewV1');
    }
    // And the wall clock stays hollow even with the page in hand.
    expect(reading.wallClock.available).toBe(false);
  });

  it('declares exactly the four plan dimensions, placement first', () => {
    expect(WORK_TOPOLOGY_DIMENSIONS).toEqual([
      'execution_placement',
      'branch_topology',
      'review_topology',
      'integration_strategy',
    ]);
  });
});
