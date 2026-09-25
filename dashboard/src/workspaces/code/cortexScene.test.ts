import { describe, expect, it } from 'vitest';

import type { GraphEdgeV1, GraphNodeV1 } from '../../contracts/generated.ts';
import {
  NO_PATH_MODULE,
  anchorsAround,
  degreeRadius,
  fitCamera,
  hubDegree,
  keyboardOrder,
  neighbourhood,
  placeLabels,
  project,
  sceneFromSlice,
  unproject,
  zoomAt,
} from './cortexScene.ts';

function node(id: string, degree: number | null, file: string | null, name = id): GraphNodeV1 {
  return { id, kind: 'function', name, file_path: file, degree } as GraphNodeV1;
}

function edge(source: string, target: string, kind = 'calls'): GraphEdgeV1 {
  return { source, target, kind, line: 1 } as GraphEdgeV1;
}

describe('sceneFromSlice', () => {
  const scene = sceneFromSlice(
    [node('a', 4, 'src/x/a.rs'), node('b', null, 'src/x/b.rs'), node('c', 2, null)],
    [edge('a', 'b'), edge('b', 'c', 'references'), edge('a', 'gone')],
  );

  it('draws only edges with both ends on the slice and counts the rest', () => {
    expect(scene.edges).toHaveLength(2);
    expect(scene.danglingEdges).toBe(1);
  });

  it('keeps an absent degree absent and a missing path printed', () => {
    expect(scene.byId.get('b')!.degree).toBeNull();
    expect(scene.unknownDegree).toBe(1);
    expect(scene.maxDegree).toBe(4);
    expect(scene.byId.get('c')!.module).toBe(NO_PATH_MODULE);
    expect(scene.byId.get('a')!.module).toBe('src/x');
  });

  it('answers neighbourhoods from drawn edges only', () => {
    expect([...neighbourhood(scene, 'b')!].sort()).toEqual(['a', 'b', 'c']);
    expect(neighbourhood(scene, 'gone')).toBeNull();
  });

  it('walks the keyboard by degree, absent degree last', () => {
    expect(keyboardOrder(scene)).toEqual(['a', 'c', 'b']);
  });
});

describe('degreeRadius', () => {
  it('makes area proportional to degree on one shared scale', () => {
    const range = { min: 2, max: 10 };
    expect(degreeRadius(null, 16, range)).toBe(2);
    expect(degreeRadius(0, 16, range)).toBe(2);
    expect(degreeRadius(4, 16, range)).toBe(6);
    expect(degreeRadius(16, 16, range)).toBe(10);
  });
});

describe('hubDegree', () => {
  it('cuts hubs at a percentile of served degree, ignoring absent degree', () => {
    const scene = sceneFromSlice(
      [...Array.from({ length: 10 }, (_, i) => node(`n${i}`, i + 1, 'src/a.rs')), node('x', null, null)],
      [],
    );
    expect(hubDegree(scene)).toBe(7);
    expect(hubDegree(scene, 0.9)).toBe(9);
    expect(hubDegree(sceneFromSlice([node('x', null, null)], []))).toBe(Infinity);
  });
});

describe('camera', () => {
  it('fits bounds into the box, centred', () => {
    const camera = fitCamera({ x0: 0, y0: 0, x1: 100, y1: 50 }, 420, 420, 10);
    expect(camera.k).toBe(4);
    expect(project(camera, 50, 25)).toEqual({ x: 210, y: 210 });
  });

  it('zooms about the pointer: the world point under it stays under it', () => {
    const camera = { k: 2, tx: 30, ty: -12 };
    const before = unproject(camera, 140, 90);
    const zoomed = zoomAt(camera, 140, 90, 1.5, { min: 0.1, max: 10 });
    expect(zoomed.k).toBe(3);
    const after = project(zoomed, before.x, before.y);
    expect(after.x).toBeCloseTo(140, 9);
    expect(after.y).toBeCloseTo(90, 9);
  });

  it('clamps zoom at its limits', () => {
    expect(zoomAt({ k: 2, tx: 0, ty: 0 }, 0, 0, 100, { min: 1, max: 4 }).k).toBe(4);
  });
});

describe('placeLabels', () => {
  it('moves a label to its next clear anchor and drops one with none', () => {
    const first = anchorsAround('a', 0, 0, 4, 40, 10);
    const second = anchorsAround('b', 0, 0, 4, 40, 10);
    const third = anchorsAround('c', 0, 0, 4, 40, 10);
    const fourth = anchorsAround('d', 0, 0, 4, 40, 10);
    const fifth = anchorsAround('e', 0, 0, 4, 40, 10);
    const kept = placeLabels([first, second, third, fourth, fifth]);
    expect(kept.map((box) => [box.id, box.x, box.y])).toEqual([
      ['a', 9, -5],
      ['b', -49, -5],
      ['c', -20, -17],
      ['d', -20, 7],
    ]);
  });

  it('never covers an obstacle belonging to another symbol, only its own', () => {
    const label = { id: 'a', x: 0, y: 0, width: 10, height: 10 };
    expect(placeLabels([[label]], [{ id: 'b', x: 5, y: 5, width: 4, height: 4 }])).toEqual([]);
    expect(placeLabels([[label]], [{ id: 'a', x: 5, y: 5, width: 4, height: 4 }])).toEqual([label]);
  });
});
