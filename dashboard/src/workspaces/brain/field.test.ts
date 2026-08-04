import { describe, expect, it } from 'vitest';
import {
  columnIndexFor,
  composeRegistryField,
  indexedMass,
  recencyVitality,
  summarizeHoldings,
  vitalityHorizon,
} from './field.ts';
import {
  type ProjectRegistryEntry,
  type ProjectRepoGroup,
} from '../../contracts/generated.ts';

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
  it('preserves an explicit zero indexed mass instead of making it one', () => {
    const field = composeRegistryField(
      [group('empty', [project('empty', 0, { stores: 0, scopes: 0, artifacts: 0 })])],
      NOW,
    );
    expect(field.nodes[0]!.degree).toBe(0);
  });

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

/**
 * The owner's real registry, in shape: forty-four projects, all seen inside ten
 * days, every one holding exactly one store, artifacts only ever 3-5, and graph
 * scopes spanning zero to two hundred and forty-two.
 */
function liveRegistry(): ProjectRepoGroup[] {
  const ages = [0.01, 0.11, 0.17, 0.19, ...Array.from({ length: 40 }, (_, i) => 1.3 + i * 0.22)];
  const scopes = [1, 0, 2, 56, 242, 36, 71, 20, 10, 8, 5, 3, 4];
  return ages.map((age, i) =>
    group(`g${i}`, [
      project(`p${i}`, age, {
        stores: 1,
        scopes: scopes[i % scopes.length]!,
        artifacts: [4, 4, 5, 3][i % 4]!,
      }),
    ]),
  );
}

describe('recencyVitality horizon', () => {
  it('uses the whole luminance scale across a ten-day registry', () => {
    const horizon = 10;
    const today = recencyVitality(NOW - 0.01 * DAY, NOW, horizon);
    const thisWeek = recencyVitality(NOW - 1.3 * DAY, NOW, horizon);
    const oldest = recencyVitality(NOW - 10 * DAY, NOW, horizon);
    expect(today).toBeGreaterThan(0.99);
    expect(oldest).toBe(0);
    // The reading the punch list called out: today and this-week were 1.00 and
    // 0.85 under the fixed 90-day horizon, a fifteen percent luminance step no
    // eye separates on a dark field. Anchored to the observed range they are
    // more than a third of the scale apart.
    expect(today - thisWeek).toBeGreaterThan(0.3);
  });

  it('still separates the two under the old fixed horizon by almost nothing', () => {
    const today = recencyVitality(NOW - 0.01 * DAY, NOW, 90);
    const thisWeek = recencyVitality(NOW - 1.3 * DAY, NOW, 90);
    expect(today - thisWeek).toBeLessThan(0.2);
  });

  it('clamps the horizon so a registry seen minutes ago is not all-or-nothing', () => {
    expect(vitalityHorizon([project('a', 0.002, { stores: 1, scopes: 1, artifacts: 1 })], NOW)).toBe(1);
    // Everything inside the clamp still reads as fully lit rather than as a
    // scale invented out of minutes of drift.
    expect(recencyVitality(NOW - 0.002 * DAY, NOW, 0.002)).toBeGreaterThan(0.99);
  });

  it('is not dragged to a century by one ancient registry entry', () => {
    const projects = [
      ...Array.from({ length: 20 }, (_, i) =>
        project(`p${i}`, 1 + i * 0.4, { stores: 1, scopes: 1, artifacts: 1 }),
      ),
      // Last seen in 2019. Taking the maximum would set the horizon at ~94
      // years and push every other body back to indistinguishable full
      // brightness — the exact compression this parameter removes.
      project('ancient', 34_000, { stores: 1, scopes: 1, artifacts: 1 }),
    ];
    const horizon = vitalityHorizon(projects, NOW);
    expect(horizon).toBeLessThan(12);
    expect(recencyVitality(NOW - 34_000 * DAY, NOW, horizon)).toBe(0);
    // ...and the bulk of the registry still spreads across the scale.
    expect(
      recencyVitality(NOW - 1 * DAY, NOW, horizon) -
        recencyVitality(NOW - 8 * DAY, NOW, horizon),
    ).toBeGreaterThan(0.3);
  });

  it('never stretches the scale past the quarter the field calls dormant', () => {
    const projects = Array.from({ length: 10 }, (_, i) =>
      project(`p${i}`, 200 + i * 50, { stores: 1, scopes: 1, artifacts: 1 }),
    );
    expect(vitalityHorizon(projects, NOW)).toBe(90);
  });

  it('composes the field with that horizon and reports it', () => {
    const field = composeRegistryField(liveRegistry(), NOW);
    expect(field.vitalityHorizonDays).toBeGreaterThan(7);
    expect(field.vitalityHorizonDays).toBeLessThan(10);
    const vitalities = field.nodes.map((node) => node.vitality);
    expect(Math.max(...vitalities)).toBeGreaterThan(0.99);
    expect(Math.min(...vitalities)).toBe(0);
  });
});

describe('mass axis frame', () => {
  it('reports the lopsided distribution instead of leaving the axis to explain itself', () => {
    const field = composeRegistryField(liveRegistry(), NOW);
    expect(field.mass.total).toBe(44);
    const masses = liveRegistry().flatMap((g) => g.projects).map(indexedMass);
    expect(field.mass.floor).toBe(Math.min(...masses));
    expect(field.mass.ceiling).toBe(Math.max(...masses));
    expect(field.mass.ceiling).toBeGreaterThan(field.mass.median * 5);
    // Most of the registry lives in the lower half of the log axis; that is the
    // reading the caption has to carry.
    expect(field.mass.lowerHalfCount).toBeGreaterThan(field.mass.total / 2);
  });

  it('frames the y axis from the bodies at its ends rather than a flat allowance', () => {
    const field = composeRegistryField(liveRegistry(), NOW);
    const [low, high] = field.extent.y;
    const ys = field.nodes.map((node) => node.y);
    // Every body is inside the frame...
    expect(low).toBeLessThan(Math.min(...ys));
    expect(high).toBeGreaterThan(Math.max(...ys));
    // ...and the frame no longer spends the old flat 0.55 on clearance at
    // either end, which is what made the axis top out empty.
    expect(high - Math.max(...ys)).toBeLessThan(0.55);
    expect(Math.min(...ys) - low).toBeLessThan(0.55);
  });
});

describe('summarizeHoldings', () => {
  const projects = liveRegistry().flatMap((g) => g.projects);

  it('states the constant channels once and keeps the one that varies', () => {
    const holdings = summarizeHoldings(projects)!;
    expect(holdings.total).toBe(44);
    expect(holdings.stores.uniform).toBe(1);
    expect(holdings.artifacts.uniform).toBeNull();
    expect(holdings.artifacts.min).toBe(3);
    expect(holdings.artifacts.max).toBe(5);
    // Scopes are the only channel with real range, and the only one that
    // belongs on a row.
    expect(holdings.scopes.uniform).toBeNull();
    expect(holdings.scopes.max - holdings.scopes.min).toBeGreaterThan(200);
    expect(holdings.uniformLine).toContain('exactly 1 store');
    expect(holdings.uniformLine).toContain('3–5 artifacts');
    expect(holdings.uniformLine).not.toContain('scope');
  });

  it('has no uniform line when every channel genuinely varies', () => {
    const holdings = summarizeHoldings([
      project('a', 1, { stores: 1, scopes: 1, artifacts: 1 }),
      project('b', 1, { stores: 9, scopes: 40, artifacts: 30 }),
    ])!;
    expect(holdings.uniformLine).toBeNull();
  });

  it('has nothing to summarize for an empty registry', () => {
    expect(summarizeHoldings([])).toBeNull();
  });
});
