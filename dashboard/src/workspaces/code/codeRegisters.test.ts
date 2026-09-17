import { describe, expect, it } from 'vitest';

import type {
  CodeIndexFreshnessPayloadV1,
  DashboardEnvelopeV1,
  GraphOverviewPayloadV1,
} from '../../contracts/generated.ts';
import { graphRegister, indexRegister, selectionRegister } from './codeRegisters.ts';

function envelope<T>(payload: T, domainState: DashboardEnvelopeV1<T>['domain_state']): DashboardEnvelopeV1<T> {
  return {
    schema_revision: 1,
    domain_state: domainState,
    payload,
    authorization: { state: 'granted' } as unknown as DashboardEnvelopeV1<T>['authorization'],
    coverage: {
      completeness: 'complete',
      denominator: null,
      eligible: null,
      examined: null,
      excluded: null,
      matched: null,
      omission_reasons: [],
      omitted: null,
      unit: null,
      unknown: null,
    },
    freshness: { observed_at_micros: null, state: 'fresh', watermark: null },
    legal_actions: [],
    scope: { project_id: null, storage_mode: 'local', store_root: '/tmp' },
    source_watermark: null,
    time: { observation_time_micros: 0, valid_time_micros: null },
    version: { entity_version: null, graph_version: null },
  };
}

describe('the graph register', () => {
  it('reports the envelope state with the symbol total, or the failure it got', () => {
    const overview = {
      totals: { nodes: 48_612, edges: 1, files: 1 },
      nodes_by_kind: [],
      edges_by_kind: [],
      files_by_language: [],
      largest_files: [],
      path: '',
      top_connected: [],
    } satisfies GraphOverviewPayloadV1;
    expect(graphRegister(false, { outcome: 'envelope', envelope: envelope(overview, 'ready') })).toEqual({
      id: 'code:graph',
      label: 'Graph',
      value: 'ready',
      state: 'ready',
      detail: '48,612 symbols',
    });
    expect(graphRegister(true, undefined)).toMatchObject({ value: 'loading', state: 'loading' });
    expect(graphRegister(false, undefined)).toMatchObject({ value: 'unknown', state: 'unknown' });
    expect(
      graphRegister(false, { outcome: 'transport', state: 'error', detail: 'HTTP 500' }),
    ).toMatchObject({ value: 'error', state: 'error', detail: 'HTTP 500' });
  });
});

describe('the index register', () => {
  const worktree = {
    worktree_root: '/fast/projects/tracedecay',
    repository_id: null,
    worktree_id: null,
    source_reference: 'refs/heads/main',
    source_revision: null,
    latest_generation_id: 'generation.1',
    snapshot_content_identity: null,
    sealed_at_micros: Date.UTC(2026, 4, 9, 14, 36, 59) * 1000,
    last_reconcile_micros: null,
    staleness_state: 'fresh',
    rebuild_in_flight: false,
    hook_hint_count: null,
    coverage: 'complete',
    progress: null,
    parked: null,
  } as unknown as CodeIndexFreshnessPayloadV1['worktrees'][number];

  it('leads with the scheduler’s staleness word and the sealed clock', () => {
    expect(
      indexRegister(false, {
        outcome: 'envelope',
        envelope: envelope({ worktrees: [worktree], note: 'n' }, 'ready'),
      }),
    ).toEqual({
      id: 'code:index',
      label: 'Index',
      value: 'fresh',
      state: 'ready',
      detail: 'sealed 14:36:59 UTC',
    });
  });

  it('counts further worktrees and names an unsealed generation', () => {
    const second = { ...worktree, worktree_root: '/other', sealed_at_micros: null, staleness_state: null };
    const register = indexRegister(false, {
      outcome: 'envelope',
      envelope: envelope({ worktrees: [second, worktree], note: 'n' }, 'partial'),
    });
    expect(register.value).toBe('partial');
    expect(register.detail).toBe('no sealed generation · +1 more worktree');
  });

  it('carries the route’s own note when no worktree is mounted', () => {
    expect(
      indexRegister(false, {
        outcome: 'envelope',
        envelope: envelope({ worktrees: [], note: 'no scheduler registry attached' }, 'unsupported'),
      }),
    ).toMatchObject({ value: 'unsupported', state: 'unsupported', detail: 'no scheduler registry attached' });
  });
});

describe('the selection register', () => {
  it('names the pinned symbol in the identity tone, and none otherwise', () => {
    expect(selectionRegister(null)).toMatchObject({ value: 'none', state: 'unknown' });
    expect(
      selectionRegister({ id: 'sym-1', name: 'subgraph_payload', kind: 'function' }),
    ).toEqual({
      id: 'code:selection',
      label: 'Selection',
      value: 'subgraph_payload',
      state: 'identity',
      detail: 'function',
    });
  });
});
