import { describe, expect, it } from 'vitest';
import {
  branchCountOf,
  composeDeliveryField,
  latestSeen,
} from './field.ts';
import {
  type ProjectRegistryEntry,
  type ProjectRepoGroup,
} from '../../contracts/generated.ts';

const NOW = 1_784_958_000;
const DAY = 86_400;

function checkout(overrides: Partial<ProjectRegistryEntry> = {}): ProjectRegistryEntry {
  return {
    project_id: 'proj_1',
    label: 'repo',
    project_root: '/src/repo',
    canonical_root: '/src/repo',
    kind: 'primary',
    default_branch: 'main',
    branches: [],
    store_count: 1,
    graph_scope_count: 1,
    artifact_count: 1,
    alias_count: 1,
    last_seen_at: NOW - 3600,
    ...overrides,
  };
}

function group(overrides: Partial<ProjectRepoGroup> = {}): ProjectRepoGroup {
  return {
    label: 'repo',
    git_common_dir: '/src/repo/.git',
    project_count: 1,
    branches: ['main'],
    projects: [checkout()],
    ...overrides,
  };
}

describe('branchCountOf', () => {
  it('counts the union of branches for a real git checkout', () => {
    expect(branchCountOf(group({ branches: ['main', 'dev', 'fix/a'] }))).toBe(3);
  });

  it('is unknown — not zero — when the entry has no git directory', () => {
    expect(branchCountOf(group({ git_common_dir: null, branches: [] }))).toBeNull();
    expect(branchCountOf(group({ git_common_dir: '', branches: [] }))).toBeNull();
  });

  it('is a real zero when a git checkout genuinely has no branches indexed', () => {
    expect(branchCountOf(group({ git_common_dir: '/x/.git', branches: [] }))).toBe(0);
  });
});

describe('latestSeen', () => {
  it('takes the liveliest checkout, because a repo is as live as its best copy', () => {
    expect(
      latestSeen(
        group({
          projects: [
            checkout({ project_id: 'a', last_seen_at: NOW - 10 * DAY }),
            checkout({ project_id: 'b', last_seen_at: NOW - DAY }),
          ],
        }),
      ),
    ).toBe(NOW - DAY);
  });
});

describe('composeDeliveryField', () => {
  it('places repositories in the shared recency columns', () => {
    const field = composeDeliveryField(
      [
        group({ label: 'today', git_common_dir: '/a/.git', projects: [checkout({ last_seen_at: NOW - 3600 })] }),
        group({ label: 'week', git_common_dir: '/b/.git', projects: [checkout({ last_seen_at: NOW - 3 * DAY })] }),
        group({ label: 'dormant', git_common_dir: '/c/.git', projects: [checkout({ last_seen_at: NOW - 200 * DAY })] }),
      ],
      NOW,
    );
    const columnOf = (label: string) =>
      field.bodies.find((body) => body.label === label)?.column;
    expect(columnOf('today')).toBe(0);
    expect(columnOf('week')).toBe(1);
    expect(columnOf('dormant')).toBe(4);
    expect(field.columns.map((column) => column.count)).toEqual([1, 1, 0, 0, 1]);
  });

  it('scales the branch axis logarithmically between the real floor and ceiling', () => {
    const field = composeDeliveryField(
      [
        group({ label: 'big', git_common_dir: '/a/.git', branches: many(242) }),
        group({ label: 'small', git_common_dir: '/b/.git', branches: many(1) }),
        group({ label: 'mid', git_common_dir: '/c/.git', branches: many(16) }),
      ],
      NOW,
    );
    expect(field.branchCeiling).toBe(242);
    expect(field.branchFloor).toBe(1);
    const yOf = (label: string) => field.bodies.find((b) => b.label === label)?.y;
    expect(yOf('big')).toBe(1);
    expect(yOf('small')).toBe(0);
    // Log, not linear: 16 of 242 is far above the 6.6% a linear axis would give.
    expect(yOf('mid')!).toBeGreaterThan(0.4);
    expect(yOf('mid')!).toBeLessThan(0.7);
  });

  it('gives a non-git entry no branch position at all rather than the axis floor', () => {
    const field = composeDeliveryField(
      [
        group({ label: 'plain', git_common_dir: null, branches: [] }),
        group({ label: 'git', git_common_dir: '/a/.git', branches: many(4) }),
      ],
      NOW,
    );
    const plain = field.bodies.find((body) => body.label === 'plain');
    expect(plain?.branches).toBeNull();
    expect(plain?.y).toBeNull();
    expect(field.unknownBranchCount).toBe(1);
    // The floor belongs to the smallest measured repository, not to the
    // unmeasured one.
    expect(field.branchFloor).toBe(4);
  });

  it('counts worktrees and multi-checkout repositories from the real kinds', () => {
    const field = composeDeliveryField(
      [
        group({
          label: 'multi',
          git_common_dir: '/a/.git',
          projects: [
            checkout({ project_id: 'p', kind: 'primary' }),
            checkout({ project_id: 'w1', kind: 'worktree' }),
            checkout({ project_id: 'w2', kind: 'worktree' }),
          ],
        }),
        group({ label: 'single', git_common_dir: '/b/.git' }),
      ],
      NOW,
    );
    const multi = field.bodies.find((body) => body.label === 'multi');
    expect(multi?.checkouts).toBe(3);
    expect(multi?.worktrees).toBe(2);
    expect(field.multiCheckoutCount).toBe(1);
    expect(field.totalCheckouts).toBe(4);
    expect(field.totalWorktrees).toBe(2);
  });

  it('sizes bodies by checkout count against the largest', () => {
    const field = composeDeliveryField(
      [
        group({
          label: 'multi',
          git_common_dir: '/a/.git',
          projects: [checkout({ project_id: 'p' }), checkout({ project_id: 'w' })],
        }),
        group({ label: 'single', git_common_dir: '/b/.git' }),
      ],
      NOW,
    );
    expect(field.bodies.find((b) => b.label === 'multi')?.size).toBe(1);
    expect(field.bodies.find((b) => b.label === 'single')?.size).toBeCloseTo(
      Math.SQRT1_2,
      5,
    );
  });

  it('brightens recent repositories and dims dormant ones', () => {
    const field = composeDeliveryField(
      [
        group({ label: 'fresh', git_common_dir: '/a/.git', projects: [checkout({ last_seen_at: NOW })] }),
        group({ label: 'old', git_common_dir: '/b/.git', projects: [checkout({ last_seen_at: NOW - 120 * DAY })] }),
      ],
      NOW,
    );
    expect(field.bodies.find((b) => b.label === 'fresh')?.vitality).toBe(1);
    expect(field.bodies.find((b) => b.label === 'old')?.vitality).toBe(0);
  });

  it('keeps every offset inside its own column, so a body never lies about when it was seen', () => {
    const groups = Array.from({ length: 20 }, (_, index) =>
      group({
        label: `r${index}`,
        git_common_dir: `/r${index}/.git`,
        branches: many(5),
        projects: [checkout({ project_id: `p${index}`, last_seen_at: NOW - 3600 })],
      }),
    );
    const field = composeDeliveryField(groups, NOW);
    for (const body of field.bodies) {
      expect(Math.abs(body.offset)).toBeLessThanOrEqual(0.4);
      expect(body.column).toBe(0);
    }
  });

  it('is deterministic — the same registry in any order composes identically', () => {
    const groups = [
      group({ label: 'a', git_common_dir: '/a/.git', branches: many(3) }),
      group({ label: 'b', git_common_dir: '/b/.git', branches: many(9) }),
      group({ label: 'c', git_common_dir: null, branches: [] }),
    ];
    const forward = composeDeliveryField(groups, NOW);
    const reversed = composeDeliveryField([...groups].reverse(), NOW);
    expect(reversed.bodies).toEqual(forward.bodies);
    expect(reversed.columns).toEqual(forward.columns);
  });

  it('orders bodies most recently indexed first', () => {
    const field = composeDeliveryField(
      [
        group({ label: 'old', git_common_dir: '/a/.git', projects: [checkout({ last_seen_at: NOW - 5 * DAY })] }),
        group({ label: 'new', git_common_dir: '/b/.git', projects: [checkout({ last_seen_at: NOW - 60 })] }),
      ],
      NOW,
    );
    expect(field.bodies.map((body) => body.label)).toEqual(['new', 'old']);
  });

  it('composes an empty registry without throwing', () => {
    const field = composeDeliveryField([], NOW);
    expect(field.bodies).toEqual([]);
    expect(field.branchCeiling).toBe(0);
    expect(field.branchFloor).toBe(0);
    expect(field.columns).toHaveLength(5);
  });

  it('marks the active project from the checkout flag', () => {
    const field = composeDeliveryField(
      [
        group({
          label: 'active',
          git_common_dir: '/a/.git',
          projects: [checkout({ is_active: true })],
        }),
        group({ label: 'idle', git_common_dir: '/b/.git' }),
      ],
      NOW,
    );
    expect(field.bodies.find((b) => b.label === 'active')?.active).toBe(true);
    expect(field.bodies.find((b) => b.label === 'idle')?.active).toBe(false);
  });
});

function many(count: number): string[] {
  return Array.from({ length: count }, (_, index) => `branch-${index}`);
}
