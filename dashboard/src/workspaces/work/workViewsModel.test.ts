import { describe, expect, it } from 'vitest';
import type { WorkProjection } from '../../contracts/index.ts';
import {
  absentChannel,
  channelGap,
  type WorkChannelGap,
  workCausalReading,
  workDagReading,
  workWeaveReading,
  workloadReading,
} from './workViewsModel.ts';

/**
 * The four projections, falsified against graphs whose answers are known by
 * hand.
 *
 * The assertions that matter most are the negative ones. Every projection here
 * is missing at least one channel plan 11c asks it to encode, and the danger is
 * not that a gap renders badly — it is that a gap quietly acquires a value.
 * So each reading is checked to report its absent channels as absent, and the
 * causal readings are checked to keep "both finished, order unread" separate
 * from "consistent", which is the one collapse that would turn an unknown into
 * a claim of agreement.
 */

function projection(overrides: Partial<WorkProjection> = {}): WorkProjection {
  return {
    accepted_proposal: null,
    authority: {
      actor_id: 'actor',
      policy_digest: 'digest',
      project_id: 'project',
      repository_id: 'repository',
      worktree_id: 'worktree',
    },
    dependencies: [],
    execution_admitted: false,
    history_len: 1,
    runtime_evidence: [],
    task_accepted: false,
    task_id: 'task',
    title: 'Task',
    version: 1,
    ...overrides,
  };
}

function evidence(runId: string, terminal: boolean) {
  return { run_id: runId, evidence_digest: `digest-${runId}`, terminal };
}

describe('the declared dependency strata', () => {
  it('layers a chain one stratum per hop', () => {
    const reading = workDagReading([
      projection({ task_id: 'a', title: 'A' }),
      projection({ task_id: 'b', title: 'B', dependencies: ['a'] }),
      projection({ task_id: 'c', title: 'C', dependencies: ['b'] }),
    ]);

    expect(reading.nodes.get('a')?.depth).toBe(0);
    expect(reading.nodes.get('b')?.depth).toBe(1);
    expect(reading.nodes.get('c')?.depth).toBe(2);
    expect(reading.strata.map((stratum) => stratum.depth)).toEqual([0, 1, 2]);
  });

  /** The longest path, not the shortest: a task that can be reached by two
   * routes sits below the deeper of them, or the stratum above it would
   * contain something it depends on. */
  it('takes the deeper of two routes to the same task', () => {
    const reading = workDagReading([
      projection({ task_id: 'a', title: 'A' }),
      projection({ task_id: 'b', title: 'B', dependencies: ['a'] }),
      projection({ task_id: 'c', title: 'C', dependencies: ['a', 'b'] }),
    ]);

    expect(reading.nodes.get('c')?.depth).toBe(2);
    expect(reading.longestChain.map((component) => component.taskIds)).toEqual([
      ['a'],
      ['b'],
      ['c'],
    ]);
  });

  it('condenses a dependency cycle into one stratum and marks its edges as climbs', () => {
    const reading = workDagReading([
      projection({ task_id: 'a', title: 'A', dependencies: ['b'] }),
      projection({ task_id: 'b', title: 'B', dependencies: ['a'] }),
      projection({ task_id: 'c', title: 'C', dependencies: ['a'] }),
    ]);

    expect(reading.cycles).toHaveLength(1);
    expect(reading.cycles[0]?.taskIds).toEqual(['a', 'b']);
    expect(reading.nodes.get('a')?.depth).toBe(reading.nodes.get('b')?.depth);
    expect(reading.nodes.get('a')?.cyclic).toBe(true);
    expect(reading.nodes.get('c')?.cyclic).toBe(false);

    const climbs = reading.edges.filter((edge) => edge.climb);
    expect(climbs).toHaveLength(2);
    // The edge out of the cycle crosses strata and is not a climb.
    expect(reading.edges.find((edge) => edge.dependent === 'c')?.climb).toBe(false);
  });

  it('treats a self-dependency as a cycle rather than a layering', () => {
    const reading = workDagReading([projection({ task_id: 'a', dependencies: ['a'] })]);

    expect(reading.edges).toEqual([{ dependency: 'a', dependent: 'a', climb: true }]);
    expect(reading.nodes.get('a')?.depth).toBe(0);
  });

  /** A capped snapshot returns some of the tasks. An edge to a task it did not
   * return must be neither layered nor dropped. */
  it('lists a dependency the snapshot did not return instead of layering it', () => {
    const reading = workDagReading([
      projection({ task_id: 'b', title: 'B', dependencies: ['a-offpage'] }),
    ]);

    expect(reading.unresolved).toEqual([{ dependency: 'a-offpage', dependent: 'b' }]);
    expect(reading.edges).toHaveLength(0);
    expect(reading.nodes.get('b')?.depth).toBe(0);
    expect(reading.nodes.has('a-offpage')).toBe(false);
  });

  it('reads an empty snapshot as an empty graph rather than failing', () => {
    const reading = workDagReading([]);

    expect(reading.strata).toHaveLength(0);
    expect(reading.longestChain).toHaveLength(0);
    expect(reading.widestStratum).toBe(0);
  });

  it('never reports an effort-weighted critical path, which no contract carries', () => {
    const reading = workDagReading([projection({ task_id: 'a' })]);

    expect(reading.effort.available).toBe(false);
  });

  /** Iterative Tarjan: the recursion depth would otherwise be the depth of the
   * dependency chain, which is data rather than something the module bounds. */
  it('layers a chain far deeper than a recursive walk would survive', () => {
    const deep = Array.from({ length: 5_000 }, (_, index) =>
      projection({
        task_id: `task-${index}`,
        dependencies: index === 0 ? [] : [`task-${index - 1}`],
      }),
    );

    const reading = workDagReading(deep);

    expect(reading.nodes.get('task-4999')?.depth).toBe(4_999);
    expect(reading.longestChain).toHaveLength(5_000);
  });
});

describe('the attempt weave', () => {
  it('weaves runs across the tasks they attached evidence to', () => {
    const reading = workWeaveReading([
      projection({ task_id: 'a', title: 'A', runtime_evidence: [evidence('run-1', true)] }),
      projection({
        task_id: 'b',
        title: 'B',
        runtime_evidence: [evidence('run-1', false), evidence('run-2', true)],
      }),
    ]);

    expect(reading.threads.map((thread) => thread.runId)).toEqual(['run-1', 'run-2']);
    expect(reading.threads[0]?.landings.map((landing) => landing.taskId)).toEqual(['a', 'b']);
    expect(reading.crossings).toBe(3);
  });

  /** A retry is a repeated crossing of the same landing, and the count is the
   * only thing that says so — there is no second timestamp to separate them. */
  it('counts a repeated crossing of one landing as a retry', () => {
    const reading = workWeaveReading([
      projection({
        task_id: 'a',
        runtime_evidence: [evidence('run-1', false), evidence('run-1', true)],
      }),
    ]);

    expect(reading.threads[0]?.landings).toHaveLength(1);
    expect(reading.threads[0]?.landings[0]?.crossings).toBe(2);
    expect(reading.threads[0]?.landings[0]?.terminal).toBe(true);
    expect(reading.threads[0]?.terminalLandings).toBe(1);
  });

  it('bands a task no run has landed on rather than omitting it', () => {
    const reading = workWeaveReading([
      projection({ task_id: 'a', title: 'A', runtime_evidence: [evidence('run-1', true)] }),
      projection({ task_id: 'z', title: 'Z' }),
    ]);

    expect(reading.unwoven).toEqual([{ taskId: 'z', title: 'Z' }]);
    expect(reading.threads.flatMap((thread) => thread.landings)).toHaveLength(1);
  });

  it('reports wall clock and executor identity as absent, never as a value', () => {
    const reading = workWeaveReading([projection({ task_id: 'a' })]);

    expect(reading.wallClock.available).toBe(false);
    expect(reading.executorIdentity.available).toBe(false);
  });
});

describe('the causal disagreement field', () => {
  it('calls out a dependent that finished while its dependency had not', () => {
    const reading = workCausalReading([
      projection({ task_id: 'a', title: 'A' }),
      projection({
        task_id: 'b',
        title: 'B',
        dependencies: ['a'],
        runtime_evidence: [evidence('run-1', true)],
      }),
    ]);

    expect(reading.disagreements).toEqual([
      { dependency: 'a', dependent: 'b', kind: 'dependent_ahead' },
    ]);
    expect(reading.counts.dependent_ahead).toBe(1);
  });

  /**
   * The collapse this test exists to prevent. Two finished tasks with no
   * timestamp between them are unordered, and reporting that as agreement
   * would be the projection inventing the very measurement it lacks.
   */
  it('keeps two finished ends unordered rather than calling them consistent', () => {
    const reading = workCausalReading([
      projection({ task_id: 'a', runtime_evidence: [evidence('run-1', true)] }),
      projection({
        task_id: 'b',
        dependencies: ['a'],
        runtime_evidence: [evidence('run-2', true)],
      }),
    ]);

    expect(reading.counts.order_unread).toBe(1);
    expect(reading.counts.consistent).toBe(0);
    expect(reading.disagreements).toHaveLength(0);
  });

  it('reads a finished dependency and an unfinished dependent as consistent so far', () => {
    const reading = workCausalReading([
      projection({ task_id: 'a', runtime_evidence: [evidence('run-1', true)] }),
      projection({ task_id: 'b', dependencies: ['a'] }),
    ]);

    expect(reading.counts.consistent).toBe(1);
  });

  it('reads two unfinished ends as unobserved rather than as agreement', () => {
    const reading = workCausalReading([
      projection({ task_id: 'a' }),
      projection({ task_id: 'b', dependencies: ['a'] }),
    ]);

    expect(reading.counts.unobserved).toBe(1);
    expect(reading.counts.consistent).toBe(0);
  });

  it('reads an off-page dependency as unresolved rather than as satisfied', () => {
    const reading = workCausalReading([
      projection({ task_id: 'b', dependencies: ['a-offpage'] }),
    ]);

    expect(reading.counts.unresolved).toBe(1);
  });

  /** Non-terminal evidence is not a finish. A task with attempts recorded and
   * none of them terminal has observed nothing yet. */
  it('does not treat non-terminal evidence as a finish', () => {
    const reading = workCausalReading([
      projection({ task_id: 'a' }),
      projection({
        task_id: 'b',
        dependencies: ['a'],
        runtime_evidence: [evidence('run-1', false)],
      }),
    ]);

    expect(reading.counts.unobserved).toBe(1);
    expect(reading.counts.dependent_ahead).toBe(0);
  });

  it('never claims to have surveyed for undeclared coupling', () => {
    const reading = workCausalReading([projection({ task_id: 'a' })]);

    expect(reading.observedOrder.available).toBe(false);
    expect(reading.undeclared.available).toBe(false);
    expect(reading.declared).toBe(0);
  });
});

describe('the workload aggregation', () => {
  it('aggregates runs into regions sized by the tasks they touched', () => {
    const reading = workloadReading([
      projection({ task_id: 'a', runtime_evidence: [evidence('run-1', true)] }),
      projection({
        task_id: 'b',
        runtime_evidence: [evidence('run-1', false), evidence('run-2', true)],
      }),
    ]);

    expect(reading.regions).toEqual([
      { runId: 'run-1', taskCount: 2, evidenceCount: 2, terminalCount: 1 },
      { runId: 'run-2', taskCount: 1, evidenceCount: 1, terminalCount: 1 },
    ]);
    expect(reading.taskCount).toBe(2);
    expect(reading.evidenceCount).toBe(3);
  });

  it('holds a task no run can be named for in the unattributed band', () => {
    const reading = workloadReading([
      projection({ task_id: 'a', runtime_evidence: [evidence('run-1', true)] }),
      projection({ task_id: 'z', title: 'Z' }),
    ]);

    expect(reading.unattributed).toEqual([{ taskId: 'z', title: 'Z' }]);
    expect(reading.regions).toHaveLength(1);
    expect(reading.taskCount).toBe(2);
  });

  it('reports mass, concurrency and churn as absent rather than as zero', () => {
    const reading = workloadReading([projection({ task_id: 'a' })]);

    expect(reading.effortMass.available).toBe(false);
    expect(reading.concurrency.available).toBe(false);
    expect(reading.churn.available).toBe(false);
  });
});

describe('the absent channels', () => {
  const gaps: readonly WorkChannelGap[] = [
    'effort',
    'wall_clock',
    'observed_order',
    'concurrency',
    'churn',
  ];

  /** A channel that says only "unavailable" is not much better than a blank.
   * Each one has to name the measurement it could not take. */
  it.each(gaps)('explains why %s could not be measured', (gap) => {
    const { detail } = channelGap(gap);

    expect(detail.length).toBeGreaterThan(40);
    expect(absentChannel(gap).available).toBe(false);
  });

  it('reports every gap as a schema absence rather than a transport failure', () => {
    for (const gap of gaps) {
      expect(channelGap(gap).state).toBe('unsupported_schema');
    }
  });
});

/**
 * The seam where the execution record meets the weave.
 *
 * The weave keeps its own reading of the snapshot whatever the attempt list
 * says — threads and landings are the snapshot's incidence — and the four
 * attempt-derived channels resolve independently. What is asserted here is that
 * the resolution is honest in both directions: a channel goes live only when a
 * page proved it, and when no page arrived the channel names the state the read
 * returned rather than reporting a schema absence for a measurement the
 * contract plainly carries.
 */
describe('the attempt channels bound onto the weave', () => {
  const GRAPH = [
    projection({ task_id: 'a', title: 'A', runtime_evidence: [evidence('run-1', true)] }),
  ];

  it('reports a read that has not answered as loading, not as unsupported', () => {
    const reading = workWeaveReading(GRAPH);

    expect(reading.executorIdentity.available).toBe(false);
    expect(reading.executorIdentity.available === false && reading.executorIdentity.state).toBe(
      'loading',
    );
    // The snapshot's own reading is unaffected by the attempt read's state.
    expect(reading.threads).toHaveLength(1);
  });

  it('carries a refusal through to every channel it fed, with its own state', () => {
    const reading = workWeaveReading(GRAPH, {
      state: 'refused',
      chip: 'conflicting',
      detail: 'the task moved since it was read',
    });

    for (const channel of [
      reading.executorIdentity,
      reading.observedOrder,
      reading.retryWeave,
      reading.cancellationLadder,
    ]) {
      expect(channel.available).toBe(false);
      expect(channel.available === false && channel.state).toBe('conflicting');
      expect(channel.available === false && channel.detail).toContain('the task moved');
    }
  });

  it('goes live on a page that proved the measurement', () => {
    const reading = workWeaveReading(GRAPH, {
      state: 'listed',
      page: {
        topology: { generation: 'generation-7', task_count: 1 },
        coverage: { coverage: 'complete', returned: 1 },
        attemptCount: 1,
        partial: false,
        executors: [
          { providerId: 'codex', routeId: 'route-primary', attempts: 1, diverted: 0, unobserved: 0 },
        ],
        lineages: [
          { taskId: 'a', runId: 'run-1', links: [], restarts: 0, open: false, truncated: false },
        ],
        ladder: { requested: 0, acknowledged: 0, escalated: 0, unrecorded: 0 },
        terminalOrder: [],
      },
    });

    expect(reading.executorIdentity.available).toBe(true);
    expect(reading.retryWeave.available).toBe(true);
    // A ladder every attempt stayed off is a reading; an empty terminal order
    // is the absence of one, because an attempt that never terminated records
    // no instant to place.
    expect(reading.cancellationLadder.available).toBe(true);
    expect(reading.observedOrder.available).toBe(false);
    expect(reading.observedOrder.available === false && reading.observedOrder.state).toBe(
      'complete_zero_findings',
    );
  });

  /** The attempt page brought an end instant and no start, so the one channel
   * that must NOT go live is the one a reader would most expect to. */
  it('leaves wall clock absent even with a full page in hand', () => {
    const reading = workWeaveReading(GRAPH, {
      state: 'listed',
      page: {
        topology: { generation: 'generation-7', task_count: 1 },
        coverage: { coverage: 'complete', returned: 0 },
        attemptCount: 0,
        partial: false,
        executors: [],
        lineages: [],
        ladder: { requested: 0, acknowledged: 0, escalated: 0, unrecorded: 0 },
        terminalOrder: [],
      },
    });

    expect(reading.wallClock.available).toBe(false);
    expect(reading.wallClock.available === false && reading.wallClock.detail).toContain(
      'never a width',
    );
  });
});
