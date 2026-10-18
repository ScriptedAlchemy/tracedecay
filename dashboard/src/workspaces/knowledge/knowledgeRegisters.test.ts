import { describe, expect, it } from 'vitest';

import type { DashboardEnvelopeV1, MemoryOverviewPayloadV1 } from '../../contracts/generated.ts';
import { cameraRegister, graphRegister, memoryRegister } from './knowledgeRegisters.ts';

function envelope<T>(payload: T, domainState: DashboardEnvelopeV1<T>['domain_state'] = 'ready'): DashboardEnvelopeV1<T> {
  return {
    schema_revision: 1,
    scope: { project_id: 'p', storage_mode: 'profile_sharded', store_root: '/data' },
    version: { entity_version: null, graph_version: null },
    time: { valid_time_micros: null, observation_time_micros: 1 },
    source_watermark: null,
    authorization: { outcome: 'authorized' },
    coverage: {
      completeness: 'complete',
      eligible: 1,
      examined: 1,
      matched: 1,
      excluded: 0,
      omitted: 0,
      unknown: 0,
      denominator: 1,
      unit: 'facts',
      omission_reasons: [],
    },
    freshness: { state: 'fresh', observed_at_micros: 1, watermark: null },
    domain_state: domainState,
    legal_actions: [],
    payload,
  } as DashboardEnvelopeV1<T>;
}

function overview(over: Partial<MemoryOverviewPayloadV1['holographic']> = {}): MemoryOverviewPayloadV1 {
  return {
    query: '',
    limit: 100,
    providers: {},
    holographic: {
      path: '/tmp/memory.db',
      exists: true,
      error: '',
      overview: { facts: 4128, entities: 612, categories: [], trust_histogram: [], growth: [] },
      facts: [],
      entities: [],
      graph: {
        nodes: [],
        edges: [],
        coverage: {
          completeness: 'unknown',
          eligible: null,
          examined: null,
          matched: null,
          excluded: null,
          omitted: null,
          unknown: null,
          denominator: null,
          unit: null,
          omission_reasons: ['fact_universe_bounded'],
        },
        fact_universe_count: 4128,
        fact_candidates_examined: 0,
        unavailable_fact_candidates: 0,
        root_count: 28,
        relation_count: 41,
        relation_limit: 100,
      },
      reads: {
        facts: { state: 'ready' },
        entities: { state: 'ready' },
        graph: { state: 'partial', code: 'graph_coverage_incomplete' },
      },
      facts_coverage: { completeness: 'partial', limit: 100 },
      ...over,
    },
  };
}

describe('knowledge registers', () => {
  it('reports the overview envelope in its own state with the store total as the qualifier', () => {
    expect(memoryRegister(true, undefined)).toMatchObject({ label: 'Memory', state: 'loading' });
    expect(memoryRegister(false, undefined)).toMatchObject({ state: 'unknown' });
    expect(memoryRegister(false, { outcome: 'transport', state: 'offline' })).toMatchObject({
      value: 'offline',
      state: 'offline',
    });
    expect(memoryRegister(false, { outcome: 'envelope', envelope: envelope(overview()) })).toMatchObject({
      value: 'ready',
      state: 'ready',
      detail: '4,128 facts',
    });
    expect(
      memoryRegister(false, { outcome: 'envelope', envelope: envelope(overview({ error: 'store locked' }), 'partial') }),
    ).toMatchObject({ state: 'partial', detail: 'store locked' });
  });

  it('reports each sub-read in the daemon state and code, never above the envelope that carries it', () => {
    const served = { outcome: 'envelope', envelope: envelope(overview()) } as const;
    expect(graphRegister(false, served)).toMatchObject({
      label: 'Graph',
      state: 'partial',
      detail: '28 roots · 41 relations',
    });
    const complete = {
      outcome: 'envelope',
      envelope: envelope(
        overview({ reads: { facts: { state: 'ready' }, entities: { state: 'ready' }, graph: { state: 'ready' } } }),
      ),
    } as const;
    expect(graphRegister(false, complete)).toMatchObject({
      state: 'ready',
      detail: '28 roots · 41 relations',
    });
    expect(graphRegister(false, { outcome: 'transport', state: 'denied' })).toMatchObject({ state: 'denied' });
    expect(graphRegister(true, undefined)).toMatchObject({ state: 'loading' });
    const unreported = {
      outcome: 'envelope',
      envelope: envelope(overview({ reads: {} })),
    } as const;
    expect(graphRegister(false, unreported)).toMatchObject({ state: 'unknown', detail: 'sub-read not reported' });
  });

  it('reports the camera as an identity, not a source state', () => {
    expect(cameraRegister('geometry')).toEqual({
      id: 'knowledge:camera',
      label: 'Camera',
      value: 'Geometry',
      state: 'identity',
    });
  });
});
