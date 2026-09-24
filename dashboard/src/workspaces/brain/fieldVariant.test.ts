import { describe, expect, it } from 'vitest';
import type { ProjectRegistryEntry, ProjectRepoGroup } from '../../contracts/generated.ts';
import type { LiveActivityPulse } from '../../data/sse/connect.ts';
import { composeRegistryField } from './field.ts';
import {
  buildGraphScene,
  buildRegistryScene,
  parseFieldVariant,
  strikeFor,
  traversalOrder,
} from './fieldVariant.ts';

const NOW = 1_800_000_000;
const DAY = 86_400;

function project(id: string, ageDays: number, stores: number, artifacts: number, branch: string | null = 'main'): ProjectRegistryEntry {
  return {
    project_id: id,
    label: id,
    project_root: `/repos/${id}`,
    canonical_root: `/repos/${id}`,
    kind: 'primary',
    default_branch: branch,
    branches: branch ? [branch] : [],
    store_count: stores,
    artifact_count: artifacts,
    alias_count: 1,
    last_seen_at: NOW - ageDays * DAY,
  };
}

const GROUPS: ProjectRepoGroup[] = [
  {
    label: 'core',
    git_common_dir: '/repos/core/.git',
    project_count: 2,
    branches: ['main'],
    projects: [project('core', 0.1, 3, 7), project('core-wt', 3, 1, 4)],
  },
  { label: 'notes', git_common_dir: null, project_count: 1, branches: [], projects: [project('notes', 40, 1, 1, null)] },
];

function scene() {
  return buildRegistryScene(composeRegistryField(GROUPS, NOW), GROUPS, NOW);
}

function pulse(projectId: string | null, family = 'tool_call_activity'): LiveActivityPulse {
  return { eventId: 'e1', observationTime: '1', projectId, family, streamId: 'tool_call', at: 0 };
}

describe('parseFieldVariant', () => {
  it('accepts only the three explored renderers', () => {
    expect(parseFieldVariant('points')).toBe('points');
    expect(parseFieldVariant('atlas')).toBe('atlas');
    expect(parseFieldVariant('sigma')).toBe('sigma');
    expect(parseFieldVariant('three')).toBeNull();
    expect(parseFieldVariant(null)).toBeNull();
  });
});

describe('buildRegistryScene', () => {
  it('draws one body per project and one massless hub per shared repository', () => {
    const built = scene();
    expect(built.bodies.map((body) => [body.id, body.role, body.mass])).toEqual([
      ['core', 'body', 10],
      ['core-wt', 'body', 5],
      ['notes', 'body', 2],
      ['repo:/repos/core/.git', 'hub', null],
    ]);
    expect(built.bodies.find((body) => body.role === 'hub')?.radius).toBe(0.07);
    expect(built.paths).toEqual([
      { source: 'repo:/repos/core/.git', target: 'core', relation: 'checkout', grade: 'EXACT' },
      { source: 'repo:/repos/core/.git', target: 'core-wt', relation: 'checkout', grade: 'EXACT' },
    ]);
  });

  it('prints absent readings instead of blanking them', () => {
    const notes = scene().bodies.find((body) => body.id === 'notes');
    expect(notes?.detail).toEqual([
      'stores 1',
      'artifacts 1',
      'mass 2',
      'seen 1mo ago',
      'branch absent',
      'repository absent',
    ]);
    expect(notes?.group).toBeNull();
  });
});

describe('strikeFor', () => {
  it('lights the touched checkout and its repository hub, never the sibling', () => {
    expect(strikeFor(pulse('core'), scene())).toEqual({
      touched: 'core',
      hop: ['repo:/repos/core/.git'],
      energy: 0.7,
      label: 'tool call',
    });
  });

  it('has no hop for a project without a drawn relation', () => {
    expect(strikeFor(pulse('notes'), scene())?.hop).toEqual([]);
  });

  it('never treats liveness, unscoped events, hubs or undrawn projects as activity', () => {
    const built = scene();
    expect(strikeFor(pulse('core', 'heartbeat'), built)).toBeNull();
    expect(strikeFor(pulse(null), built)).toBeNull();
    expect(strikeFor(pulse('repo:/repos/core/.git'), built)).toBeNull();
    expect(strikeFor(pulse('elsewhere'), built)).toBeNull();
  });
});

describe('traversalOrder', () => {
  it('walks recency columns left to right, heavier first, hubs skipped', () => {
    expect(traversalOrder(scene())).toEqual(['core', 'core-wt', 'notes']);
  });
});

describe('buildGraphScene', () => {
  it('keeps an absent connectedness absent and drops relations to undrawn symbols', () => {
    const built = buildGraphScene(
      [
        { id: 'a', label: 'a', kind: 'function', degree: 4, x: 0, y: 0 },
        { id: 'b', label: 'b', kind: 'struct', x: 1, y: 1 },
      ],
      [
        { source: 'a', target: 'b', kind: 'calls' },
        { source: 'a', target: 'ghost', kind: 'calls' },
      ],
    );
    expect(built.bodies.map((body) => body.detail)).toEqual([
      ['function', 'connectedness 4'],
      ['struct', 'connectedness absent'],
    ]);
    expect(built.paths).toEqual([{ source: 'a', target: 'b', relation: 'calls', grade: 'EXACT' }]);
    expect(built.columns).toBeNull();
  });
});
