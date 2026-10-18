import { describe, expect, it } from 'vitest';
import { WorkGraphReadV1Schema } from '../../contracts/generated.ts';
import { workGraphRead } from '../../test/workGraphFixture.ts';
import { workTaskView } from '../../test/workTaskViewFixture.ts';
import { workGraphReading } from './workGraphModel.ts';
import { workDagReading } from './workViewsModel.ts';
import type { WorkTaskView } from './workProductView.ts';

/**
 * The DAG projection, falsified against graphs whose answers are known by hand.
 *
 * The assertions that matter most are the negative ones: the projection is
 * missing at least one channel it is asked to encode, and the danger is not
 * that a gap renders badly, it is that a gap quietly acquires a value.
 */

function projection(overrides: Partial<WorkTaskView> = {}): WorkTaskView {
  return workTaskView(overrides);
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

/**
 * The graph channels bound onto the DAG.
 *
 * Every graph read here is built by the shared wire fixture and parsed through
 * `WorkGraphReadV1Schema`, the same schema `callWork` parses the wire with,
 * so these bindings and the DOM tests cannot drift into two ideas of the
 * contract. The separations under proof: declared-and-empty from absent,
 * withheld from zero.
 */
describe('the graph channels bound onto the DAG', () => {
  function graphOf(spec: Parameters<typeof workGraphRead>[0]) {
    return workGraphReading({
      outcome: 'value',
      value: WorkGraphReadV1Schema.parse(workGraphRead(spec)),
    });
  }

  it('weights the critical path with the authority\'s own chain and effort', () => {
    const reading = workDagReading(
      [projection({ task_id: 'a' })],
      graphOf({
        tasks: [{ taskId: 'a', effort: 3 }, { taskId: 'b', effort: 5 }],
        criticalPath: ['a', 'b'],
      }),
    );

    expect(reading.effort.available).toBe(true);
    expect(reading.effort.available && reading.effort.value).toEqual({
      taskIds: ['a', 'b'],
      totalEffort: 8,
    });
    // The unweighted chain over THIS page's edges survives beside it.
    expect(reading.longestChain.map((component) => component.taskIds)).toEqual([['a']]);
  });

  it('reads an empty gating set as the plan gating nothing, not as an absence', () => {
    const reading = workDagReading([projection({ task_id: 'a' })], graphOf({
      tasks: [{ taskId: 'a' }],
    }));

    expect(reading.gating.available).toBe(true);
    expect(reading.gating.available && reading.gating.value).toEqual([]);
  });
});
