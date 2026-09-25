import { describe, expect, it } from 'vitest';
import { MAX_POINTS, createPicker, pointCloud, unitsPerPoint } from './pointField.ts';
import {
  bodyScreenRadius,
  createSpatialIndex,
  fitCamera,
  resolveZoom,
  toScreen,
  toWorld,
  zoomAt,
  type FieldScene,
  type SceneBody,
  type SceneCluster,
} from './scene.ts';

function body(id: string, x: number, y: number, extra: Partial<SceneBody> = {}): SceneBody {
  return {
    id,
    label: id,
    role: 'body',
    kind: 'primary',
    x,
    y,
    radius: 0.1,
    mass: 10,
    units: { stores: 3, artifacts: 7 },
    vitality: 1,
    detail: ['stores 3', 'artifacts 7', 'mass 10', 'seen 1h ago', 'branch main', 'repo r'],
    group: null,
    cluster: null,
    ...extra,
  };
}

function registry(bodies: SceneBody[], clusters: SceneCluster[] = []): FieldScene {
  return {
    bodies,
    paths: [],
    clusters,
    extent: { x: [-0.5, 4.5], y: [0, 3] },
    columns: ['today', 'week', 'month', 'quarter', 'dormant'].map((label) => ({ label, bound: label, count: 0 })),
    neighbors: new Map(),
  };
}

const NO_PAD = { top: 0, right: 0, bottom: 0, left: 0 };

describe('field camera', () => {
  it('fits the extent inside the padded viewport, centred', () => {
    const camera = fitCamera({ x: [0, 10], y: [0, 5] }, 1000, 600, NO_PAD);
    expect(camera.scale).toBe(100);
    expect(toScreen(camera, 0, 5)).toEqual([0, 50]);
    expect(toScreen(camera, 10, 0)).toEqual([1000, 550]);
  });

  it('zooms around the pointer, keeping the world point under it fixed', () => {
    const camera = fitCamera({ x: [0, 10], y: [0, 5] }, 1000, 600, NO_PAD);
    const before = toWorld(camera, 250, 300);
    const zoomed = zoomAt(camera, 250, 300, 2);
    expect(zoomed.scale).toBe(200);
    expect(toWorld(zoomed, 250, 300)).toEqual(before);
  });

  it('grows bodies slower than positions so packed members separate', () => {
    expect(bodyScreenRadius(0.1, 100, 100)).toBe(10);
    expect(bodyScreenRadius(0.1, 800, 100)).toBeCloseTo(16.82, 2);
    expect(resolveZoom(0.1, 0.5)).toBe(1);
    expect(resolveZoom(0.24, 0.115)).toBeCloseTo(6.72, 2);
  });
});

describe('point field', () => {
  it('draws one point per indexed unit, stores first, inside the unit disc', () => {
    const cloud = pointCloud({ stores: 3, artifacts: 7 }, 1);
    expect(cloud.offsets.length).toBe(20);
    expect(cloud.stores).toBe(3);
    for (let index = 0; index < 10; index += 1) {
      expect(Math.hypot(cloud.offsets[index * 2]!, cloud.offsets[index * 2 + 1]!)).toBeLessThanOrEqual(1);
    }
  });

  it('shares one units-per-point ratio once a registry exceeds the point budget', () => {
    expect(unitsPerPoint(registry([body('a', 0, 1)]))).toBe(1);
    const heavy = registry([body('a', 0, 1, { units: { stores: 1, artifacts: MAX_POINTS * 3 } })]);
    expect(unitsPerPoint(heavy)).toBe(4);
    expect(pointCloud({ stores: 1, artifacts: MAX_POINTS * 3 }, 4).offsets.length / 2).toBe(1 + MAX_POINTS * 0.75);
  });

  it('picks the body under the pointer and nothing in empty space', () => {
    const scene = registry([body('a', 0, 1), body('b', 3, 2)]);
    const camera = fitCamera(scene.extent, 1000, 600, NO_PAD);
    const picker = createPicker(scene);
    const [ax, ay] = toScreen(camera, 0, 1);
    expect(picker.pick(camera, camera.scale, ax + 3, ay - 2)).toEqual({ kind: 'body', body: scene.bodies[0] });
    expect(picker.pick(camera, camera.scale, ax + 200, ay)).toBeNull();
  });

  it('picks an unresolved cell as a whole, and its member once zoomed past the resolve zoom', () => {
    const cell: SceneCluster = { id: 'cell:0:0', members: ['a', 'b'], x: 0, y: 1, width: 0.8, height: 0.25, resolveZoom: 4, mass: 20, spacing: 0.1 };
    const scene = registry([body('a', -0.05, 1, { cluster: cell.id }), body('b', 0.05, 1, { cluster: cell.id })], [cell]);
    const fit = fitCamera(scene.extent, 1000, 600, NO_PAD);
    const picker = createPicker(scene);
    const [cx, cy] = toScreen(fit, -0.05, 1);
    expect(picker.pick(fit, fit.scale, cx, cy)).toEqual({ kind: 'cluster', cluster: cell });
    const close = zoomAt(fit, cx, cy, 5);
    expect(picker.pick(close, fit.scale, cx, cy)).toEqual({ kind: 'body', body: scene.bodies[0] });
  });

  it('answers a pick from a few index buckets among thousands of bodies', () => {
    const bodies = Array.from({ length: 20_000 }, (_, index) => body(`p${index}`, (index % 200) * 0.3, Math.floor(index / 200) * 0.3));
    const index = createSpatialIndex(bodies, 0.25);
    const near = index.near(3, 3, 0.2);
    expect(near.map((item) => item.id).sort()).toEqual(['p2010']);
    const scene = { ...registry(bodies), extent: { x: [0, 60] as [number, number], y: [0, 30] as [number, number] } };
    const camera = fitCamera(scene.extent, 1200, 600, NO_PAD);
    const [x, y] = toScreen(camera, 3, 3);
    const picked = createPicker(scene).pick(camera, camera.scale, x, y);
    expect(picked?.kind === 'body' ? picked.body.id : null).toBe('p2010');
  });
});
