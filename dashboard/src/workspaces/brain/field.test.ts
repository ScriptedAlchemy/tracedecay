import { describe, expect, it } from 'vitest';
import {
  columnIndexFor,
  composeRegistryField,
  indexedMass,
  recencyVitality,
} from './field.ts';
import type { ProjectRegistryEntry, ProjectRepoGroup } from './contracts.ts';

const NOW = 1_800_000_000;
const DAY = 86_400;

function project(
  id: string,
  ageDays: number,
  mass: { stores: number; scopes: number; artifacts: number },
): ProjectRegistryEntry {
  return {
    project_id: id,
    label: id,
    project_root: `/repos/${id}`,
    canonical_root: `/repos/${id}`,
    kind: 'primary',
    default_branch: 'main',
    branches: ['main'],
    store_count: mass.stores,
    graph_scope_count: mass.scopes,
    artifact_count: mass.artifacts,
    alias_count: 1,
    last_seen_at: NOW - ageDays * DAY,
  };
}

function group(label: string, projects: ProjectRegistryEntry[]): ProjectRepoGroup {
  return {
    label,
    git_common_dir: `/repos/${label}/.git`,
    project_count: projects.length,
    branches: ['main'],
    projects,
  };
}

const SINGLE = { stores: 1, scopes: 1, artifacts: 1 };

describe('indexedMass', () => {
  it('sums exactly the three registry counts it names', () => {
    expect(indexedMass(project('a', 0, { stores: 2, scopes: 3, artifacts: 7 }))).toBe(12);
  });
});

describe('columnIndexFor', () => {
  it('places each age in the column whose bound it falls under', () => {
    expect(columnIndexFor(NOW - 3600, NOW)).toBe(0);
    expect(columnIndexFor(NOW - 3 * DAY, NOW)).toBe(1);
    expect(columnIndexFor(NOW - 20 * DAY, NOW)).toBe(2);
    expect(columnIndexFor(NOW - 60 * DAY, NOW)).toBe(3);
    expect(columnIndexFor(NOW - 400 * DAY, NOW)).toBe(4);
  });

  it('treats a future timestamp as this instant rather than as an error', () => {
    expect(columnIndexFor(NOW + 5 * DAY, NOW)).toBe(0);
  });
});

describe('recencyVitality', () => {
  it('burns at the moment of contact and extinguishes past the last column', () => {
    expect(recencyVitality(NOW, NOW)).toBe(1);
    expect(recencyVitality(NOW - 400 * DAY, NOW)).toBe(0);
  });

  it('decays monotonically with age', () => {
    const ages = [0, 1, 7, 30, 90].map((days) =>
      recencyVitality(NOW - days * DAY, NOW),
    );
    for (let i = 1; i < ages.length; i += 1) {
      expect(ages[i]!).toBeLessThan(ages[i - 1]!);
    }
  });
});

describe('composeRegistryField', () => {
  it('draws one body per project and no synthetic hub for a lone checkout', () => {
    const field = composeRegistryField(
      [
        group('alpha', [project('alpha', 0, SINGLE)]),
        group('beta', [project('beta', 10, SINGLE)]),
      ],
      NOW,
    );
    expect(field.nodes.map((n) => n.id).sort()).toEqual(['alpha', 'beta']);
    // The old composition paired each project with a repository hub, which made
    // every repository an isolated two-node component and handed the renderer a
    // ring of pairs to pack. A repository with one checkout IS that checkout.
    expect(field.edges).toEqual([]);
    expect(field.sharedRepoCount).toBe(0);
  });

  it('materialises a hub only where several checkouts really share a git dir', () => {
    const field = composeRegistryField(
      [
        group('shared', [
          project('shared-main', 0, SINGLE),
          project('shared-wt', 2, SINGLE),
        ]),
        group('lonely', [project('lonely', 1, SINGLE)]),
      ],
      NOW,
    );
    expect(field.sharedRepoCount).toBe(1);
    expect(field.edges).toHaveLength(2);
    expect(field.edges.every((edge) => edge.source === 'repo:/repos/shared/.git')).toBe(
      true,
    );
    expect(field.edges.map((edge) => edge.target).sort()).toEqual([
      'shared-main',
      'shared-wt',
    ]);
    // The hub sits at its checkouts' centroid — the honest position for a node
    // that means "these ones".
    const hub = field.nodes.find((node) => node.id.startsWith('repo:'))!;
    const kids = field.nodes.filter((node) => node.id.startsWith('shared-'));
    expect(hub.x).toBeCloseTo((kids[0]!.x + kids[1]!.x) / 2, 6);
    expect(hub.y).toBeCloseTo((kids[0]!.y + kids[1]!.y) / 2, 6);
  });

  it('places a project in the column its last contact actually falls in', () => {
    const field = composeRegistryField(
      [
        group('now', [project('now', 0, SINGLE)]),
        group('old', [project('old', 200, SINGLE)]),
      ],
      NOW,
    );
    const byId = new Map(field.nodes.map((node) => [node.id, node]));
    expect(Math.round(byId.get('now')!.x)).toBe(0);
    expect(Math.round(byId.get('old')!.x)).toBe(4);
  });

  it('never lets an anti-overlap nudge move a body into a neighbouring column', () => {
    // Twelve projects with identical recency AND identical mass: every one of
    // them wants the same point, so this is the worst case the offset search
    // can be handed.
    const crowd = Array.from({ length: 12 }, (_, i) =>
      project(`p${i}`, 0.1, SINGLE),
    );
    const field = composeRegistryField(
      crowd.map((entry) => group(entry.project_id, [entry])),
      NOW,
    );
    for (const node of field.nodes) {
      expect(Math.abs(node.x - 0)).toBeLessThan(0.5);
    }
  });

  it('reads mass as height, heaviest highest', () => {
    const field = composeRegistryField(
      [
        group('heavy', [project('heavy', 0, { stores: 4, scopes: 40, artifacts: 200 })]),
        group('light', [project('light', 0, SINGLE)]),
      ],
      NOW,
    );
    const byId = new Map(field.nodes.map((node) => [node.id, node]));
    // Sigma's y grows downward, so "higher on screen" is the LARGER y here.
    expect(byId.get('heavy')!.y).toBeGreaterThan(byId.get('light')!.y);
    expect(byId.get('heavy')!.degree).toBeGreaterThan(byId.get('light')!.degree);
  });

  it('counts every project into exactly one column', () => {
    const entries = [0, 3, 20, 60, 400].map((days, i) =>
      group(`g${i}`, [project(`p${i}`, days, SINGLE)]),
    );
    const field = composeRegistryField(entries, NOW);
    expect(field.columns.map((column) => column.count)).toEqual([1, 1, 1, 1, 1]);
    expect(
      field.columns.reduce((sum, column) => sum + column.count, 0),
    ).toBe(entries.length);
  });

  it('frames the whole axis, so an unoccupied column stays visible as empty', () => {
    const field = composeRegistryField(
      [group('only', [project('only', 0, SINGLE)])],
      NOW,
    );
    expect(field.extent.x[0]).toBeLessThan(0);
    // Four column gaps wide even though only the first column has anything in
    // it: the four empty columns are the reading.
    expect(field.extent.x[1]).toBeGreaterThan(4);
    expect(field.columns.filter((column) => column.count === 0)).toHaveLength(4);
  });

  it('is deterministic for the same payload and clock', () => {
    const groups = [0, 0, 0, 5, 5, 40].map((days, i) =>
      group(`g${i}`, [project(`p${i}`, days, { stores: 1, scopes: i, artifacts: i * 2 })]),
    );
    const a = composeRegistryField(groups, NOW);
    const b = composeRegistryField([...groups].reverse(), NOW);
    const key = (field: ReturnType<typeof composeRegistryField>) =>
      [...field.nodes]
        .sort((l, r) => l.id.localeCompare(r.id))
        .map((node) => `${node.id}@${node.x.toFixed(6)},${node.y.toFixed(6)}`);
    // Independent of the order repositories arrive in, too — the registry is
    // re-sorted on every render and the picture must not jump.
    expect(key(a)).toEqual(key(b));
  });

  it('composes an empty registry without inventing a body', () => {
    const field = composeRegistryField([], NOW);
    expect(field.nodes).toEqual([]);
    expect(field.edges).toEqual([]);
    expect(field.columns.every((column) => column.count === 0)).toBe(true);
  });
});
