import { describe, expect, it } from 'vitest';
import { kindAtlas, pickPlate, registryAtlas } from './atlas.ts';
import { MAX_POINTS, pickBody, pointCloud, unitsPerPoint } from './pointField.ts';
import { fitCamera, toScreen, toWorld, zoomAt, type FieldScene, type SceneBody } from './scene.ts';

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
    ...extra,
  };
}

function registry(bodies: SceneBody[], paths: FieldScene['paths'] = []): FieldScene {
  const neighbors = new Map<string, string[]>();
  for (const path of paths) {
    neighbors.set(path.source, [...(neighbors.get(path.source) ?? []), path.target]);
    neighbors.set(path.target, [...(neighbors.get(path.target) ?? []), path.source]);
  }
  return {
    bodies,
    paths,
    extent: { x: [-0.5, 4.5], y: [0, 3] },
    columns: ['today', 'week', 'month', 'quarter', 'dormant'].map((label) => ({ label, bound: label, count: 0 })),
    neighbors,
  };
}

describe('field camera', () => {
  it('fits the extent inside the padded viewport, centred', () => {
    const camera = fitCamera({ x: [0, 10], y: [0, 5] }, 1000, 600, { top: 0, right: 0, bottom: 0, left: 0 });
    expect(camera.scale).toBe(100);
    expect(toScreen(camera, 0, 5)).toEqual([0, 50]);
    expect(toScreen(camera, 10, 0)).toEqual([1000, 550]);
  });

  it('zooms around the pointer, keeping the world point under it fixed', () => {
    const camera = fitCamera({ x: [0, 10], y: [0, 5] }, 1000, 600, { top: 0, right: 0, bottom: 0, left: 0 });
    const before = toWorld(camera, 250, 300);
    const zoomed = zoomAt(camera, 250, 300, 2);
    expect(zoomed.scale).toBe(200);
    expect(toWorld(zoomed, 250, 300)).toEqual(before);
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
    const small = registry([body('a', 0, 1)]);
    expect(unitsPerPoint(small)).toBe(1);
    const heavy = registry([body('a', 0, 1, { units: { stores: 1, artifacts: MAX_POINTS * 3 } })]);
    expect(unitsPerPoint(heavy)).toBe(4);
    expect(pointCloud({ stores: 1, artifacts: MAX_POINTS * 3 }, 4).offsets.length / 2).toBe(1 + MAX_POINTS * 0.75);
  });

  it('picks the body under the pointer and nothing in empty space', () => {
    const scene = registry([body('a', 0, 1), body('b', 3, 2)]);
    const camera = fitCamera(scene.extent, 1000, 600, { top: 0, right: 0, bottom: 0, left: 0 });
    const [ax, ay] = toScreen(camera, 0, 1);
    expect(pickBody(scene, camera, ax + 3, ay - 2)?.id).toBe('a');
    expect(pickBody(scene, camera, ax + 200, ay)).toBeNull();
  });
});

describe('instrument atlas', () => {
  it('orders plates by canonical id inside their recency column', () => {
    const scene = registry([body('zeta', 0, 2.5), body('alpha', 0, 0.2), body('mid', 2, 1)]);
    const layout = registryAtlas(scene, 1000, 600);
    expect(layout.plates.map((plate) => [plate.id, plate.x === layout.columnX[0], plate.y])).toEqual([
      ['alpha', true, 44],
      ['zeta', true, 120],
      ['mid', false, 44],
    ]);
    expect(layout.tier.level).toBe('full');
  });

  it('steps down to smaller plate tiers instead of shrinking text', () => {
    const crowded = registry(Array.from({ length: 10 }, (_, index) => body(`p${String(index).padStart(2, '0')}`, 0, 1)));
    expect(registryAtlas(crowded, 1000, 600).tier.level).toBe('compact');
    expect(registryAtlas(crowded, 1000, 360).tier.level).toBe('minimal');
  });

  it('routes a shared repository through a gutter bus between member plates', () => {
    const hub = body('repo:r', 0.5, 1, { role: 'hub', mass: null, units: null });
    const scene = registry([body('a', 0, 1), body('b', 1, 1), hub], [
      { source: 'repo:r', target: 'a', relation: 'checkout', grade: 'EXACT' },
      { source: 'repo:r', target: 'b', relation: 'checkout', grade: 'EXACT' },
    ]);
    const layout = registryAtlas(scene, 1000, 600);
    expect(layout.plates.map((plate) => plate.id)).toEqual(['a', 'b']);
    expect(layout.wires).toHaveLength(2);
    const [a, b] = layout.plates;
    const bus = a!.x + a!.w + 9;
    expect(layout.wires.map((wire) => wire.points[1]![0])).toEqual([bus, bus]);
    expect(b!.x).toBeGreaterThan(bus);
    expect(pickPlate(layout, a!.x + 5, a!.y + 5)).toBe('a');
    expect(pickPlate(layout, bus, a!.y + 5)).toBeNull();
  });

  it('groups a returned graph into kind plates with printed relation counts', () => {
    const scene: FieldScene = {
      ...registry([
        body('f1', 0, 0, { kind: 'function', mass: 3 }),
        body('f2', 0, 0, { kind: 'function', mass: null }),
        body('s1', 0, 0, { kind: 'struct', mass: 5 }),
      ]),
      columns: null,
      paths: [
        { source: 'f1', target: 's1', relation: 'calls', grade: 'EXACT' },
        { source: 'f2', target: 's1', relation: 'calls', grade: 'EXACT' },
        { source: 'f1', target: 'f2', relation: 'calls', grade: 'EXACT' },
      ],
    };
    const layout = kindAtlas(scene, 800);
    expect(layout.plates.map((plate) => [plate.title, plate.footer, plate.rows?.map((row) => row.id)])).toEqual([
      ['function · 2 symbols', 'relations  struct 2', ['f1', 'f2']],
      ['struct · 1 symbol', 'relations  function 2', ['s1']],
    ]);
    const row = layout.plates[0]!.rows![1]!;
    expect(pickPlate(layout, layout.plates[0]!.x + 20, row.y - 4)).toBe('f2');
  });
});
