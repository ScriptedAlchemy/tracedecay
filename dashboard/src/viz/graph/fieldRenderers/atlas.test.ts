import { describe, expect, it } from 'vitest';
import { kindAtlas, pickPlate, registryAtlas } from './atlas.ts';
import { buildGraphScene, buildRegistryScene } from '../../../workspaces/brain/registryScene.ts';
import { composeRegistryField } from '../../../workspaces/brain/field.ts';
import type { ProjectRegistryEntry, ProjectRepoGroup } from '../../../contracts/generated.ts';

const NOW = 1_800_000_000;
const project = (id: string, age: number): ProjectRegistryEntry => ({
  project_id: id,
  label: id,
  project_root: `/${id}`,
  canonical_root: `/${id}`,
  kind: 'primary',
  store_count: 2,
  artifact_count: 8,
  alias_count: 0,
  branches: ['main'],
  default_branch: 'main',
  head_branch: 'main',
  last_seen_at: NOW - age * 86_400,
});
const projects = [project('recent', 0.1), project('week', 3), project('month', 20), project('quarter', 70), project('old', 200)];
const groups: ProjectRepoGroup[] = [{ label: 'repo', git_common_dir: '/repo/.git', branches: ['main'], project_count: projects.length, projects }];

describe('restored Brain atlas', () => {
  it.each([240, 800, 1440])('keeps every project readable and selectable at %i pixels', (width) => {
    const scene = buildRegistryScene(composeRegistryField(groups, NOW), groups, NOW);
    const layout = registryAtlas(scene, width, 500);
    expect(layout.plates.map((plate) => plate.id).sort()).toEqual(projects.map((item) => item.project_id).sort());
    for (const plate of layout.plates) {
      expect(plate.w).toBeGreaterThanOrEqual(190);
      expect(plate.x).toBeGreaterThanOrEqual(0);
      expect(plate.x + plate.w).toBeLessThanOrEqual(width);
      expect(pickPlate(layout, plate.x + plate.w / 2, plate.y + plate.h / 2)).toBe(plate.id);
    }
    expect(pickPlate(layout, 0, 0)).toBeNull();
  });

  it('retains every returned symbol and picks its exact identity inside a kind plate', () => {
    const nodes = Array.from({ length: 40 }, (_, index) => ({ id: `symbol-${index}`, label: `symbol-${index}`, kind: 'function', degree: index, x: index, y: 0 }));
    const scene = buildGraphScene(nodes, [{ source: 'symbol-0', target: 'symbol-1', kind: 'calls' }]);
    const layout = kindAtlas(scene, 320);
    const plate = layout.plates[0]!;
    expect(plate.rows).toHaveLength(nodes.length);
    for (const row of plate.rows ?? []) {
      expect(pickPlate(layout, plate.x + 20, row.y - 4)).toBe(row.id);
    }
  });
});
