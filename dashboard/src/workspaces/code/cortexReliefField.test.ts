import { describe, expect, it } from 'vitest';

import type { GraphEdgeV1, GraphNodeV1 } from '../../contracts/generated.ts';
import { sceneFromSlice } from './cortexScene.ts';
import {
  contourInterval,
  contourSegments,
  reliefLayout,
  reliefSurface,
} from './cortexReliefField.ts';

function node(id: string, file: string, degree = 1): GraphNodeV1 {
  return { id, kind: 'function', name: id, file_path: file, degree } as GraphNodeV1;
}

function edge(source: string, target: string): GraphEdgeV1 {
  return { source, target, kind: 'calls', line: 1 } as GraphEdgeV1;
}

describe('contourInterval', () => {
  it('picks a 1-2-5 step giving about ten levels, never below one endpoint', () => {
    expect(contourInterval(3)).toBe(1);
    expect(contourInterval(23)).toBe(5);
    expect(contourInterval(95)).toBe(10);
    expect(contourInterval(140)).toBe(20);
  });
});

describe('reliefSurface', () => {
  it('peaks at exactly the weight of an isolated endpoint', () => {
    const surface = reliefSurface([{ x: 0, y: 0, weight: 3 }], { x0: -40, y0: -40, x1: 40, y1: 40 }, 10, 5);
    expect(surface.max).toBeCloseTo(3, 9);
  });

  it('is flat zero with no weighted endpoints', () => {
    const surface = reliefSurface([{ x: 0, y: 0, weight: 0 }], { x0: -10, y0: -10, x1: 10, y1: 10 }, 5, 5);
    expect(surface.max).toBe(0);
  });

  it('rings a peak below its height and draws nothing above it', () => {
    const surface = reliefSurface([{ x: 0, y: 0, weight: 2 }], { x0: -40, y0: -40, x1: 40, y1: 40 }, 10, 4);
    expect(contourSegments(surface, 1)).toHaveLength(80);
    expect(contourSegments(surface, 2.5).length).toBe(0);
  });
});

describe('reliefLayout', () => {
  const nodes = [
    ...['a1', 'a2', 'a3', 'a4', 'a5'].map((id) => node(id, 'src/a/x.rs', 3)),
    ...['b1', 'b2', 'b3'].map((id) => node(id, 'src/b/y.rs', 2)),
    node('c1', 'lib/c/z.rs'),
  ];
  const edges = [edge('a1', 'a2'), edge('a1', 'b1'), edge('b2', 'c1'), edge('a3', 'a4')];
  const layout = reliefLayout(sceneFromSlice(nodes, edges), 2);

  it('gives each directory one module, most populous first', () => {
    expect(layout.modules.map((m) => [m.module, m.count])).toEqual([
      ['src/a', 5],
      ['src/b', 3],
      ['lib/c', 1],
    ]);
  });

  it('packs modules without overlap and keeps members inside their module', () => {
    for (const [i, a] of layout.modules.entries()) {
      for (const b of layout.modules.slice(i + 1)) {
        expect(Math.hypot(a.cx - b.cx, a.cy - b.cy)).toBeGreaterThanOrEqual(a.radius + b.radius);
      }
    }
    for (const [id, p] of layout.positions) {
      const module = layout.moduleOf.get(id)!;
      expect(Math.hypot(p.x - module.cx, p.y - module.cy)).toBeLessThanOrEqual(module.radius);
    }
  });

  it('raises each directory relief from its own symbols only', () => {
    // lib/c's one symbol carries one relation; src/a's five carry six ends.
    const heights = layout.modules.map((m) => [m.module, Number(layout.surfaces.get(m)!.max.toFixed(3))]);
    expect(heights).toEqual([
      ['src/a', 3.137],
      ['src/b', 1.603],
      ['lib/c', 0.982],
    ]);
    expect(layout.interval).toBe(1);
  });

  it('keeps a silent directory flat however busy its neighbour is', () => {
    const quiet = reliefLayout(
      sceneFromSlice(
        [...['a1', 'a2', 'a3'].map((id) => node(id, 'src/a/x.rs', 3)), node('q', 'src/q/y.rs')],
        [edge('a1', 'a2'), edge('a2', 'a3'), edge('a1', 'a3')],
      ),
      2,
    );
    const q = quiet.modules.find((m) => m.module === 'src/q')!;
    expect(quiet.surfaces.get(q)!.max).toBe(0);
    expect(quiet.surfaces.get(q)!.contours).toEqual([]);
  });

  it('aggregates relations across directories into one counted trunk per pair', () => {
    expect(layout.trunks.map((t) => [t.a.module, t.b.module, t.count])).toEqual([
      ['src/a', 'src/b', 1],
      ['lib/c', 'src/b', 1],
    ]);
  });

  it('is deterministic for the same slice', () => {
    const again = reliefLayout(sceneFromSlice(nodes, edges), 2);
    expect([...again.positions]).toEqual([...layout.positions]);
  });
});
