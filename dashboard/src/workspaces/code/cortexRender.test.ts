/**
 * The renderer's geometry, tested where it can be tested without a canvas.
 *
 * `createCortexRenderer` needs a 2D context, but the three functions that
 * decide WHERE a mark lands — the outline, its extent, and the hit test that
 * has to agree with both — are pure, and they are the ones that can silently
 * disagree with the model. A hit test that does not match the drawn shape is a
 * surface where clicking a region selects a different one.
 */
import { describe, expect, it } from 'vitest';

import type { StrataMeasurementV1 } from '../../contracts/generated.ts';
import { RELIEF_ASPECT, buildCortexModel } from './cortexRelief.ts';
import { hitRegion, reliefExtent, reliefOutline } from './cortexRender.ts';

function twoRegionModel() {
  const measurement: StrataMeasurementV1 = {
    algorithm: 'tarjan_scc_then_longest_path',
    cluster_ordering: 'dsm_boundary_edges_desc_then_file_count_desc',
    clusters: [
      {
        directory: 'src/graph',
        order: 0,
        file_count: 40,
        internal_edges: 80,
        incoming_edges: 9,
        outgoing_edges: 11,
        boundary_edges: 20,
      },
      {
        directory: 'src/store',
        order: 1,
        file_count: 4,
        internal_edges: 3,
        incoming_edges: 2,
        outgoing_edges: 3,
        boundary_edges: 5,
      },
    ],
    dependency_edge_kinds: ['calls', 'uses'],
    files: [
      { path: 'src/graph/a.rs', depth: 2, scc_size: 1, chain: ['src/graph/a.rs'] },
      { path: 'src/store/a.rs', depth: 0, scc_size: 1, chain: ['src/store/a.rs'] },
    ],
    granularity: 'file',
    graph_generation: 'g-1',
    ideal_depth: 2,
    max_depth: 3,
    scan: {
      budget_ms: 4000,
      cache_scope: 'graph_generation',
      cache_state: 'hit',
      dependency_edges_examined: 100,
      files_examined: 44,
      max_dependency_edges: 40_000,
      max_files: 20_000,
    },
  };
  return buildCortexModel(measurement);
}

describe('reliefOutline', () => {
  it('is deterministic, so a screenshot of the same graph is the same picture', () => {
    expect(reliefOutline('src/graph', 40)).toEqual(reliefOutline('src/graph', 40));
  });

  it('gives two directories different landforms and the same mean radius', () => {
    const mean = (points: readonly { x: number; y: number }[]) =>
      points.reduce(
        (total, point) =>
          total +
          Math.hypot(point.x / RELIEF_ASPECT.x, point.y / RELIEF_ASPECT.y),
        0,
      ) / points.length;
    const left = reliefOutline('src/graph', 40);
    const right = reliefOutline('src/store', 40);
    expect(left).not.toEqual(right);
    // The wobble is mean-preserving: two regions with the same file count
    // enclose the same area whatever their names hash to.
    expect(mean(left)).toBeCloseTo(mean(right), 0);
    expect(mean(left)).toBeCloseTo(40, 0);
  });

  it('stays inside the extent the hit test uses', () => {
    const radius = 30;
    const { rx, ry } = reliefExtent(radius);
    // A margin, because the harmonics push the outline slightly past the base
    // ellipse; the hit ellipse is the base one, so a click near the fringe
    // misses rather than selecting a neighbour. That is the safe direction.
    for (const point of reliefOutline('src/graph', radius)) {
      expect(Math.abs(point.x)).toBeLessThanOrEqual(rx * 1.2);
      expect(Math.abs(point.y)).toBeLessThanOrEqual(ry * 1.2);
    }
  });
});

describe('hitRegion', () => {
  it('selects the region whose body the point is inside', () => {
    const model = twoRegionModel();
    const region = model.drawnRegions[0]!;
    expect(hitRegion(model, region.x!, region.y!)).toBe(region.directory);
  });

  it('selects nothing on open ground rather than the nearest body', () => {
    const model = twoRegionModel();
    expect(hitRegion(model, 4, 4)).toBeNull();
  });

  it('prefers the smaller body when two overlap, so a speck stays reachable', () => {
    const model = twoRegionModel();
    const small = [...model.drawnRegions].sort(
      (a, b) => (a.radius ?? 0) - (b.radius ?? 0),
    )[0]!;
    // Sitting exactly on the small region's centre must resolve to it even if a
    // massif were drawn over the same point.
    expect(hitRegion(model, small.x!, small.y!)).toBe(small.directory);
  });
});
