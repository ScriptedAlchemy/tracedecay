import { describe, expect, it } from 'vitest';
import type { ProjectRegistryEntry, ProjectRepoGroup } from '../../contracts/generated.ts';
import { composeRegistryField } from '../../workspaces/brain/field.ts';
import {
  CROWN_SCALE,
  HUB_RADIUS,
  bodiesBounds,
  buildRegistryScene,
  columnSpread,
  depthFor,
  pickBody,
  samplePath,
  spreadX,
} from './registrySceneModel.ts';

const NOW = 1_700_000_000;
const DAY = 86_400;

function entry(id: string, ageDays: number, artifacts: number, kind = 'primary'): ProjectRegistryEntry {
  return {
    project_id: id,
    label: id,
    project_root: `/${id}`,
    canonical_root: `/${id}`,
    kind,
    store_count: 1,
    artifact_count: artifacts,
    alias_count: 0,
    branches: [],
    default_branch: null,
    last_seen_at: NOW - ageDays * DAY,
  };
}

function group(label: string, gitCommonDir: string | null, projects: ProjectRegistryEntry[]): ProjectRepoGroup {
  return { label, git_common_dir: gitCommonDir, branches: [], project_count: projects.length, projects };
}

const GROUPS: ProjectRepoGroup[] = [
  group('shared', '/shared/.git', [entry('main', 0.2, 300), entry('wt', 3, 40, 'worktree')]),
  group('lone', '/lone/.git', [entry('lone', 20, 12)]),
  group('old', '/old/.git', [entry('old', 400, 3)]),
];

describe('registry scene model', () => {
  const field = composeRegistryField(GROUPS, NOW);
  const scene = buildRegistryScene(field);

  it('keeps every measured coordinate and adds only the field’s own relations', () => {
    for (const node of field.nodes) {
      const body = scene.byId.get(node.id);
      expect(body).toBeDefined();
      expect(body!.x).toBe(node.x);
      expect(body!.y).toBe(node.y);
    }
    expect(scene.paths.map((path) => [path.from, path.to, path.relation, path.grade])).toEqual([
      ['main', 'repo:/shared/.git', 'checkout', 'exact'],
      ['wt', 'repo:/shared/.git', 'checkout', 'exact'],
    ]);
    // Lone checkouts have no hub and no path: nothing to conduct into.
    expect(scene.pathsByBody.has('lone')).toBe(false);
    expect(scene.pathsByBody.get('repo:/shared/.git')).toHaveLength(2);
  });

  it('gives a hub a fixed categorical size, never a holding-sized crown', () => {
    const hub = scene.byId.get('repo:/shared/.git')!;
    expect(hub.kind).toBe('repository');
    expect(hub.radius).toBe(HUB_RADIUS);
    const heavy = scene.byId.get('main')!;
    const light = scene.byId.get('old')!;
    expect(heavy.radius).toBeGreaterThan(light.radius);
    expect(light.radius).toBeGreaterThan(hub.radius);
    expect(heavy.radius).toBeCloseTo(0.24 * CROWN_SCALE, 6);
  });

  it('restates recency as depth so dormant bodies recede', () => {
    expect(depthFor(1)).toBe(0);
    expect(depthFor(0.3)).toBe(1);
    expect(depthFor(0)).toBe(2);
    expect(scene.byId.get('main')!.depth).toBe(0);
    expect(scene.byId.get('old')!.depth).toBe(2);
    expect(scene.byId.get('repo:/shared/.git')!.depth).toBe(0);
  });

  it('grows the frame to hold the tallest crown and the deepest tail', () => {
    expect(scene.extent.x).toEqual(field.extent.x);
    expect(scene.extent.y[1]).toBeGreaterThanOrEqual(field.extent.y[1]);
    expect(scene.extent.y[0]).toBeLessThanOrEqual(field.extent.y[0]);
    const heavy = scene.byId.get('main')!;
    expect(scene.extent.y[1]).toBeGreaterThanOrEqual(heavy.y + heavy.radius * 0.9);
  });

  it('prints one tick per recency column at its centre line', () => {
    expect(scene.columns.map((column) => column.x)).toEqual([0, 1, 2, 3, 4]);
    expect(scene.columnDividers).toEqual([0.5, 1.5, 2.5, 3.5]);
    expect(scene.columns.map((column) => column.count)).toEqual(field.columns.map((column) => column.count));
  });

  it('picks the smallest crown under a point and nothing over empty field', () => {
    const light = scene.byId.get('old')!;
    expect(pickBody(scene, light.x, light.y)?.id).toBe('old');
    expect(pickBody(scene, light.x + 5, light.y + 5)).toBeNull();
    const hub = scene.byId.get('repo:/shared/.git')!;
    expect(pickBody(scene, hub.x, hub.y)?.id).toBe('repo:/shared/.git');
  });

  it('spreads the categorical axis to the aperture without touching a body', () => {
    expect(columnSpread(scene.extent, { width: 400, height: 400 })).toBe(1);
    const wide = columnSpread(scene.extent, { width: 2000, height: 400 });
    expect(wide).toBeGreaterThan(1);
    expect(wide).toBeLessThanOrEqual(2.4);
    const body = scene.byId.get('lone')!;
    expect(spreadX(body.x, wide)).toBeCloseTo(body.x * wide, 12);
    // Picking answers in the spread world.
    expect(pickBody(scene, spreadX(body.x, wide), body.y, wide)?.id).toBe('lone');
  });

  it('samples an evidenced path from below the crown to the hub and sags it', () => {
    const path = scene.paths[0]!;
    const points = samplePath(scene, path, 1, 8);
    const project = scene.byId.get(path.from)!;
    const hub = scene.byId.get(path.to)!;
    expect(points).toHaveLength(9);
    expect(points[0]![0]).toBeCloseTo(project.x, 9);
    expect(points[0]![1]).toBeLessThan(project.y);
    expect(points[8]).toEqual([hub.x, hub.y]);
    const straightMid = (points[0]![1] + points[8]![1]) / 2;
    expect(points[4]![1]).toBeLessThan(straightMid);
    expect(samplePath(scene, { ...path, from: 'absent' }, 1)).toEqual([]);
  });

  it('bounds a repository neighbourhood for the camera', () => {
    const bounds = bodiesBounds(scene, ['main', 'wt', 'repo:/shared/.git'])!;
    for (const id of ['main', 'wt', 'repo:/shared/.git']) {
      const body = scene.byId.get(id)!;
      expect(body.x).toBeGreaterThanOrEqual(bounds.x[0]);
      expect(body.x).toBeLessThanOrEqual(bounds.x[1]);
      expect(body.y).toBeGreaterThanOrEqual(bounds.y[0]);
      expect(body.y).toBeLessThanOrEqual(bounds.y[1]);
    }
    expect(bodiesBounds(scene, ['absent'])).toBeNull();
  });
});
