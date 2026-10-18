import { describe, expect, it } from 'vitest';
import { workTaskView } from '../../test/workTaskViewFixture.ts';
import {
  WORK_DAG_LAYOUT_DEFAULTS,
  dagNeighborhood,
  dagPathToOutcome,
  dagPathToRoot,
  workDagLayout,
} from './workDagLayout.ts';
import type { WorkTaskView } from './workProductView.ts';
import { workDagReading } from './workViewsModel.ts';

/**
 * The layout is a pure function and these tests hold it to that: identical
 * input gives identical coordinates, depth is the strata's, order within a
 * stratum follows the predecessors, and every relation kind is either drawn
 * between two laid-out cards or reported as unresolved. Nothing is measured
 * from a DOM.
 */

const CHAIN: readonly WorkTaskView[] = [
  workTaskView({ task_id: 'root', title: 'Root' }),
  workTaskView({ task_id: 'left', title: 'Left', dependencies: ['root'] }),
  workTaskView({ task_id: 'right', title: 'Right', dependencies: ['root'] }),
  workTaskView({
    task_id: 'leaf',
    title: 'Leaf',
    dependencies: ['left', 'right'],
    informational_relations: ['root'],
    causal_candidates: ['left'],
  }),
];

function layoutOf(projections: readonly WorkTaskView[]) {
  return workDagLayout(workDagReading(projections), projections);
}

describe('workDagLayout', () => {
  it('is deterministic: the same reading yields the same coordinates', () => {
    const first = layoutOf(CHAIN);
    const second = layoutOf(CHAIN);
    expect(second.nodes).toEqual(first.nodes);
    expect(second.edges).toEqual(first.edges);
    expect(second.width).toBe(first.width);
    expect(second.height).toBe(first.height);
  });

  it('places each task on the stratum its longest dependency path gives it', () => {
    const layout = layoutOf(CHAIN);
    expect(layout.byId.get('root')?.depth).toBe(0);
    expect(layout.byId.get('left')?.depth).toBe(1);
    expect(layout.byId.get('right')?.depth).toBe(1);
    expect(layout.byId.get('leaf')?.depth).toBe(2);
    const rows = layout.strata.map((stratum) => stratum.y);
    expect(rows).toEqual([...rows].sort((a, b) => a - b));
    expect(new Set(rows).size).toBe(3);
  });

  it('centres a narrow stratum under a wide one', () => {
    const layout = layoutOf(CHAIN);
    const root = layout.byId.get('root');
    const left = layout.byId.get('left');
    const right = layout.byId.get('right');
    if (root === undefined || left === undefined || right === undefined) throw new Error('unlaid');
    const rootCentre = root.x + root.width / 2;
    const pairCentre = (left.x + right.x + right.width) / 2;
    expect(Math.abs(rootCentre - pairCentre)).toBeLessThan(0.5);
  });

  it('orders a stratum by the columns of its gating predecessors, then by identity', () => {
    const projections = [
      workTaskView({ task_id: 'a', title: 'A' }),
      workTaskView({ task_id: 'b', title: 'B' }),
      // `z` depends on `a` (column 0) and `c` depends on `b` (column 1), so the
      // barycenter puts `z` before `c` even though its identity sorts after.
      workTaskView({ task_id: 'z', title: 'Z', dependencies: ['a'] }),
      workTaskView({ task_id: 'c', title: 'C', dependencies: ['b'] }),
    ];
    const layout = layoutOf(projections);
    expect(layout.strata[1]?.taskIds).toEqual(['z', 'c']);
  });

  it('sorts an unanchored component after the anchored ones in its stratum', () => {
    const projections = [
      workTaskView({ task_id: 'a', title: 'A' }),
      workTaskView({ task_id: 'aa', title: 'AA', dependencies: ['a'] }),
      // In the root stratum beside `a` only because of the cycle it forms with
      // `cb`, which has no predecessor outside the cycle: no column anchors it.
      workTaskView({ task_id: 'ca', title: 'CA', dependencies: ['cb'] }),
      workTaskView({ task_id: 'cb', title: 'CB', dependencies: ['ca'] }),
    ];
    const layout = layoutOf(projections);
    const cycle = layout.strata.find((stratum) => stratum.taskIds.includes('ca'));
    expect(cycle?.taskIds).toEqual(['a', 'ca', 'cb']);
    expect(layout.byId.get('a')?.cyclic).toBe(false);
    expect(layout.byId.get('ca')?.cyclic).toBe(true);
    expect(layout.byId.get('cb')?.cyclic).toBe(true);
  });

  it('draws every relation kind between laid-out cards and names each by kind', () => {
    const layout = layoutOf(CHAIN);
    const kinds = layout.edges.map((edge) => `${edge.kind}:${edge.from}->${edge.to}`);
    expect(kinds).toEqual([
      'causal:left->leaf',
      'gating:left->leaf',
      'gating:right->leaf',
      'gating:root->left',
      'gating:root->right',
      'informational:leaf->root',
    ]);
    for (const edge of layout.edges) {
      expect(edge.path.startsWith('M ')).toBe(true);
      expect(edge.climb).toBe(false);
    }
  });

  it('reports a relation to a task outside the page rather than drawing it', () => {
    const projections = [
      workTaskView({ task_id: 'only', title: 'Only', informational_relations: ['elsewhere'] }),
    ];
    const layout = layoutOf(projections);
    expect(layout.edges).toEqual([]);
    expect(layout.unresolved).toEqual([{ from: 'only', to: 'elsewhere', kind: 'informational' }]);
  });

  it('marks a gating edge inside a declared cycle as a climb and curves it', () => {
    const projections = [
      workTaskView({ task_id: 'p', title: 'P', dependencies: ['q'] }),
      workTaskView({ task_id: 'q', title: 'Q', dependencies: ['p'] }),
    ];
    const layout = layoutOf(projections);
    expect(layout.edges).toHaveLength(2);
    for (const edge of layout.edges) {
      expect(edge.climb).toBe(true);
      expect(edge.path).toContain(' C ');
    }
  });

  it('gives an empty reading an empty, padded extent', () => {
    const layout = layoutOf([]);
    expect(layout.nodes).toEqual([]);
    expect(layout.width).toBe(WORK_DAG_LAYOUT_DEFAULTS.padding * 2);
    expect(layout.height).toBe(WORK_DAG_LAYOUT_DEFAULTS.padding * 2);
  });
});

describe('the focus sets over the layout', () => {
  it('isolates the one-hop neighbourhood over every drawn relation', () => {
    const layout = layoutOf(CHAIN);
    const around = dagNeighborhood(layout, 'left');
    expect([...around.tasks].sort()).toEqual(['leaf', 'left', 'root']);
    expect([...around.edges].sort()).toEqual([
      'causal:left->leaf',
      'gating:left->leaf',
      'gating:root->left',
    ]);
  });

  it('walks gating edges upstream to the root and downstream to the outcome', () => {
    const reading = workDagReading(CHAIN);
    const upstream = dagPathToRoot(reading, 'leaf');
    expect([...upstream.tasks].sort()).toEqual(['leaf', 'left', 'right', 'root']);
    expect(upstream.edges.has('gating:root->left')).toBe(true);
    expect(upstream.edges.has('informational:leaf->root')).toBe(false);

    const downstream = dagPathToOutcome(reading, 'root');
    expect([...downstream.tasks].sort()).toEqual(['leaf', 'left', 'right', 'root']);
    expect(downstream.edges.has('gating:left->leaf')).toBe(true);
  });
});
