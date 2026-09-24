import { describe, expect, it } from 'vitest';

import type { GraphEdgeV1, GraphNodeV1, StrataMeasurementV1 } from '../../contracts/generated.ts';
import { sceneFromSlice } from './cortexScene.ts';
import { edgeCourse, plateLayout, sliceDepths, stratify } from './cortexPlate.ts';

function node(id: string, file: string | null, degree = 1): GraphNodeV1 {
  return { id, kind: 'function', name: id, file_path: file, degree } as GraphNodeV1;
}

function edge(source: string, target: string, kind = 'calls'): GraphEdgeV1 {
  return { source, target, kind, line: 1 } as GraphEdgeV1;
}

function measured(files: { path: string; depth: number }[]): {
  outcome: 'measured';
  measurement: StrataMeasurementV1;
} {
  return {
    outcome: 'measured',
    measurement: {
      algorithm: 'longest_path_layering',
      cluster_ordering: 'boundary_edges_desc',
      clusters: [],
      dependency_edge_kinds: ['imports', 'calls'],
      files: files.map((file) => ({ ...file, chain: [], scc_size: 1 })),
      granularity: 'file',
      graph_generation: 'g-7',
      ideal_depth: 2,
      max_depth: 3,
      scan: {} as StrataMeasurementV1['scan'],
    },
  };
}

const chain = sceneFromSlice(
  [node('a', 'src/p/a.rs'), node('b', 'src/p/b.rs'), node('c', 'src/q/c.rs'), node('d', 'src/q/d.rs')],
  [edge('a', 'b'), edge('b', 'c', 'references'), edge('c', 'd', 'contains')],
);

describe('sliceDepths', () => {
  it('layers by the longest dependency path, ignoring containment', () => {
    const { depth, cycles, dependencyEdges } = sliceDepths(chain);
    expect(Object.fromEntries(depth)).toEqual({ a: 2, b: 1, c: 0, d: 0 });
    expect(cycles).toBe(0);
    expect(dependencyEdges).toBe(2);
  });

  it('condenses a cycle to one depth and counts it', () => {
    const cyclic = sceneFromSlice(
      [node('a', null), node('b', null), node('c', null)],
      [edge('a', 'b'), edge('b', 'a'), edge('b', 'c')],
    );
    const { depth, cycles, component } = sliceDepths(cyclic);
    expect(Object.fromEntries(depth)).toEqual({ a: 1, b: 1, c: 0 });
    expect(cycles).toBe(1);
    expect(component.get('a')).toBe(component.get('b'));
    expect(component.has('c')).toBe(false);
  });
});

describe('stratify', () => {
  it('uses the index strata when they place at least half the drawn files', () => {
    const strat = stratify(
      chain,
      measured([
        { path: 'src/p/a.rs', depth: 3 },
        { path: 'src/q/c.rs', depth: 1 },
      ]),
    );
    expect(strat.basis).toEqual({ kind: 'strata', placedFiles: 2, drawnFiles: 4, generation: 'g-7' });
    expect(Object.fromEntries(strat.depth)).toEqual({ a: 3, b: null, c: 1, d: null });
  });

  it('falls back to the slice layering and says why', () => {
    const under = stratify(chain, measured([{ path: 'src/p/a.rs', depth: 3 }]));
    expect(under.basis).toMatchObject({
      kind: 'slice',
      why: 'index strata place 1 of 4 drawn files, under half',
    });
    const pending = stratify(chain, 'pending');
    expect(pending.basis).toMatchObject({ kind: 'slice', why: 'index strata still reading' });
    const unmeasured = stratify(chain, {
      outcome: 'unmeasured',
      reason: 'not_indexed',
      detail: 'no graph generation',
    });
    expect(unmeasured.basis).toMatchObject({
      kind: 'slice',
      why: 'index strata unmeasured: not_indexed, no graph generation',
    });
  });

  it('marks a relation against the strata layering as a climb', () => {
    const strat = stratify(
      chain,
      measured([
        { path: 'src/p/a.rs', depth: 0 },
        { path: 'src/p/b.rs', depth: 2 },
      ]),
    );
    expect(edgeCourse({ strat }, 'a', 'b')).toBe('climbs');
    expect(edgeCourse({ strat }, 'b', 'c')).toBe('unplaced');
  });
});

describe('plateLayout', () => {
  it('prints unscanned symbols in their own band rather than dropping them', () => {
    const layout = plateLayout(
      chain,
      { width: 900, height: 400 },
      measured([
        { path: 'src/p/a.rs', depth: 1 },
        { path: 'src/p/b.rs', depth: 0 },
      ]),
    );
    expect(layout.bands.map((band) => [band.key, band.count])).toEqual([
      [1, 1],
      [0, 1],
      ['unplaced', 2],
    ]);
    expect(layout.positions.size).toBe(4);
  });

  it('folds a crowded cell behind an exact count', () => {
    const crowd = sceneFromSlice(
      Array.from({ length: 30 }, (_, i) => node(`n${i}`, 'src/one/f.rs')),
      [],
    );
    const layout = plateLayout(crowd, { width: 320, height: 160 }, 'pending');
    const drawn = layout.positions.size;
    const hidden = layout.folds.reduce((sum, fold) => sum + fold.hidden, 0);
    expect(layout.folds).toHaveLength(1);
    expect(drawn + hidden).toBe(30);
    expect(hidden).toBeGreaterThan(0);
  });
});
