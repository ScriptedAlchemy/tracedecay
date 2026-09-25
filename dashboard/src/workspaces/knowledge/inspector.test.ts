import { describe, expect, it } from 'vitest';

import type {
  DashboardEnvelopeV1,
  MemoryFactDetailPayloadV1,
  MemoryFactRowV1,
} from '../../contracts/generated.ts';
import type { MemoryTrustHistoryPayloadV1 } from '../../contracts/generated.ts';
import { detailLadder, payloadAccessState, type LadderInput } from './inspector.ts';

function row(over: Partial<MemoryFactRowV1> = {}): MemoryFactRowV1 {
  return {
    fact_id: 'fact-1',
    payload_access: 'eligible',
    trust_score: 0.5,
    retrieval_count: 0,
    access_count: 0,
    helpful_count: 0,
    unhelpful_count: 0,
    created_at: 1,
    updated_at: 1,
    last_recalled_at: null,
    projected_as_of: 1,
    content: 'content',
    category: 'general',
    tags: [],
    entities: [],
    metadata: {},
    source_label: null,
    linked_entities: null,
    ...over,
  };
}

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

function history(over: Partial<MemoryTrustHistoryPayloadV1> = {}): MemoryTrustHistoryPayloadV1 {
  return {
    fact_id: 'fact-1',
    trust_history: [],
    limit: 300,
    completeness: 'complete',
    next_after: null,
    error: '',
    ...over,
  };
}

const base: LadderInput = {
  mode: 'inspecting',
  row: row(),
  detail: null,
  history: null,
  relations: 2,
  graphRead: { state: 'ready' },
};

function rung(input: LadderInput, id: ReturnType<typeof detailLadder>[number]['id']) {
  const found = detailLadder(input).find((candidate) => candidate.id === id);
  if (!found) throw new Error(`missing rung ${id}`);
  return found;
}

describe('payloadAccessState', () => {
  it('maps eligible to ready and every withheld state to a typed non-ready chip', () => {
    expect(payloadAccessState('eligible').kind).toBe('ready');
    expect(payloadAccessState('redacted').kind).toBe('redacted');
    expect(payloadAccessState('quarantined').kind).toBe('locked');
    expect(payloadAccessState('deleted').kind).toBe('unavailable');
    expect(payloadAccessState('retention_expired').kind).toBe('unavailable');
    expect(payloadAccessState('unavailable').kind).toBe('unavailable');
    expect(payloadAccessState('ambiguous').kind).toBe('conflicting');
  });
});

describe('detailLadder', () => {
  it('keeps the canonical detail and trust history unknown while merely inspecting', () => {
    expect(rung(base, 'canonical_detail').state).toBe('unknown');
    expect(rung(base, 'canonical_detail').detail).toMatch(/select the fact/);
    expect(rung(base, 'trust_history').state).toBe('unknown');
    expect(rung(base, 'trust_history').detail).toBe('loads on selection');
  });

  it('reports a served canonical detail as ready and a missing identity as unavailable', () => {
    const served: LadderInput = {
      ...base,
      mode: 'selected',
      detail: {
        pending: false,
        result: { outcome: 'envelope', envelope: envelope<MemoryFactDetailPayloadV1>({ fact: row(), error: '' }) },
      },
    };
    expect(rung(served, 'canonical_detail').state).toBe('ready');
    const missing: LadderInput = {
      ...base,
      mode: 'selected',
      detail: {
        pending: false,
        result: {
          outcome: 'envelope',
          envelope: envelope<MemoryFactDetailPayloadV1 | null>(null, 'complete_zero_findings') as DashboardEnvelopeV1<MemoryFactDetailPayloadV1>,
        },
      },
    };
    expect(rung(missing, 'canonical_detail').state).toBe('unavailable');
    expect(rung(missing, 'canonical_detail').detail).toMatch(/holds no fact under this identity/);
  });

  it('reads a null-payload complete_zero_findings transport as a missing identity, not a green complete', () => {
    const missing: LadderInput = {
      ...base,
      mode: 'selected',
      detail: { pending: false, result: { outcome: 'transport', state: 'complete_zero_findings' } },
    };
    expect(rung(missing, 'canonical_detail')).toMatchObject({
      state: 'unavailable',
      detail: 'the store holds no fact under this identity in the current scope',
    });
  });

  it('carries a detail transport failure in the daemon state vocabulary', () => {
    const offline: LadderInput = {
      ...base,
      mode: 'selected',
      detail: { pending: false, result: { outcome: 'transport', state: 'offline' } },
    };
    expect(rung(offline, 'canonical_detail').state).toBe('offline');
    const loading: LadderInput = { ...base, mode: 'selected', detail: { pending: true } };
    expect(rung(loading, 'canonical_detail').state).toBe('loading');
  });

  it('reads payload access and source label from the canonical row over the bounded one', () => {
    const input: LadderInput = {
      ...base,
      mode: 'selected',
      row: row({ payload_access: 'eligible', source_label: null }),
      detail: {
        pending: false,
        result: {
          outcome: 'envelope',
          envelope: envelope<MemoryFactDetailPayloadV1>({
            fact: row({ payload_access: 'redacted', source_label: 'hook: session-ingest' }),
            error: '',
          }),
        },
      },
    };
    expect(rung(input, 'payload_access').state).toBe('redacted');
    expect(rung(input, 'source_label').state).toBe('ready');
    expect(rung(input, 'source_label').detail).toBe('hook: session-ingest');
  });

  it('states an absent source label as unknown rather than empty', () => {
    expect(rung(base, 'source_label').state).toBe('unknown');
    expect(rung(base, 'source_label').detail).toMatch(/no source label recorded/);
  });

  it('distinguishes a complete empty audit, a complete audit, a partial window, and a failed read', () => {
    const selected = (h: LadderInput['history']): LadderInput => ({ ...base, mode: 'selected', history: h });
    expect(rung(selected({ pending: true }), 'trust_history').state).toBe('loading');
    expect(
      rung(selected({ pending: false, result: { outcome: 'ok', data: history() } }), 'trust_history').state,
    ).toBe('complete_zero_findings');
    const event = {
      event_id: 'e1',
      timestamp: 1,
      action: 'helpful' as const,
      old_trust: 0.5,
      new_trust: 0.6,
      delta: 0.1,
      details_availability: 'available' as const,
    };
    expect(
      rung(
        selected({ pending: false, result: { outcome: 'ok', data: history({ trust_history: [event] }) } }),
        'trust_history',
      ).state,
    ).toBe('ready');
    expect(
      rung(
        selected({
          pending: false,
          result: {
            outcome: 'ok',
            data: history({
              trust_history: [event],
              completeness: 'partial',
              next_after: { occurred_at: 1, event_id: 'e1' },
            }),
          },
        }),
        'trust_history',
      ).state,
    ).toBe('partial');
    expect(
      rung(selected({ pending: false, result: { outcome: 'ok', data: history({ error: 'audit unreadable' }) } }), 'trust_history'),
    ).toMatchObject({ state: 'error', detail: 'audit unreadable' });
    expect(
      rung(selected({ pending: false, result: { outcome: 'error', detail: 'HTTP 500' } }), 'trust_history'),
    ).toMatchObject({ state: 'error', detail: 'HTTP 500' });
  });

  it('reports drawn relations under the graph sub-read state', () => {
    expect(rung(base, 'graph_relations')).toMatchObject({ state: 'ready', detail: '2 relations drawn' });
    expect(
      rung({ ...base, relations: null, graphRead: { state: 'partial', code: 'graph_coverage_incomplete' } }, 'graph_relations'),
    ).toMatchObject({
      state: 'partial',
      detail: 'fact is not among the drawn roots; graph_coverage_incomplete',
    });
    expect(rung({ ...base, graphRead: undefined }, 'graph_relations').state).toBe('unknown');
  });

  it('never ticks a rung no authority serves', () => {
    expect(rung(base, 'source_verification').state).toBe('unavailable');
    expect(rung(base, 'source_verification').detail).toMatch(/no verification authority/);
    expect(rung(base, 'geometry').state).toBe('unknown');
    expect(rung(base, 'geometry').detail).toMatch(/separate Geometry camera/);
  });
});
