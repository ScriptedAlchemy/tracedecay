import { describe, expect, it } from 'vitest';
import {
  DashboardEnvelopeV1Schema,
  ExplorerQueryRunV1Schema,
  ExplorerSourceProgressV1Schema,
  type DashboardEnvelopeV1,
  type ExplorerQueryRunV1,
  type ExplorerSourceProgressV1,
} from '../../contracts/generated.ts';
import type { EnvelopeResult } from '../../data/query/envelope.ts';
import {
  browseLane,
  laneEvidence,
  laneFromSourceProgress,
  laneFromTransport,
  laneHits,
  laneStateDetail,
  laneStateKind,
  runIsTerminal,
  runStateKind,
  searchLane,
  type ExplorerLaneReadModel,
} from './laneModel.ts';

const KNOWLEDGE_FACT_ID = `fact.${'a'.repeat(64)}.${'7'.padStart(64, '0')}`;

/**
 * Every source fixture is parsed through the generated schema, so a shape the
 * contract would reject cannot quietly prove anything here.
 */
function progress(over: Record<string, unknown>): ExplorerSourceProgressV1 {
  return ExplorerSourceProgressV1Schema.parse({
    source_id: 'code_graph',
    source_label: 'Code graph',
    phase: 'completed',
    outcome: 'ready',
    completed_units: 0,
    total_units: 0,
    coverage: {
      completeness: 'complete',
      eligible: 0,
      examined: 0,
      matched: 0,
      excluded: 0,
      omitted: 0,
      unknown: 0,
      denominator: 0,
      unit: 'symbols',
      omission_reasons: [],
    },
    freshness: 'unknown',
    watermark: null,
    error_code: null,
    message: null,
    page: { offset: 0, limit: 50, total: 0, next_offset: null, rows: [], metadata: {} },
    ...over,
  });
}

function run(over: Record<string, unknown> = {}): ExplorerQueryRunV1 {
  return ExplorerQueryRunV1Schema.parse({
    run_id: 'run-1',
    request: { query: 'graph', limit: 50, offset: 0 },
    request_revision: 'explorer-query-request-v1',
    plan_revision: 'explorer-query-plan-v1',
    merge_revision: 'source-local-no-merge-v1',
    required_source_ids: ['code_graph', 'sessions', 'knowledge', 'semantic'],
    ordering_policy: 'source_local_no_cross_source_merge',
    explanation: 'test',
    submitted_at_micros: 1,
    completed_at_micros: 2,
    elapsed_micros: 1,
    state: 'completed',
    finality: 'complete',
    sources: [],
    ...over,
  });
}

function answered(payload: ExplorerQueryRunV1): EnvelopeResult<ExplorerQueryRunV1> {
  return {
    outcome: 'envelope',
    envelope: DashboardEnvelopeV1Schema(ExplorerQueryRunV1Schema).parse({
      schema_revision: 1,
      scope: { project_id: 'p', storage_mode: 'profile_sharded', store_root: '/data' },
      version: { entity_version: null, graph_version: null },
      time: { valid_time_micros: null, observation_time_micros: 1 },
      source_watermark: null,
      authorization: { outcome: 'authorized' },
      coverage: {
        completeness: 'complete',
        eligible: 3,
        examined: 3,
        matched: null,
        excluded: null,
        omitted: 0,
        unknown: null,
        denominator: 3,
        unit: 'sources',
        omission_reasons: [],
      },
      freshness: { state: 'unknown', observed_at_micros: null, watermark: null },
      domain_state: 'ready',
      legal_actions: [],
      payload,
    }),
  };
}

function browseAnswered<T>(payload: T): EnvelopeResult<T> {
  const result = answered(run());
  if (result.outcome !== 'envelope') throw new Error('test envelope must be accepted');
  const envelope = result.envelope;
  return { outcome: 'envelope', envelope: { ...envelope, payload } as DashboardEnvelopeV1<T> };
}

describe('laneFromSourceProgress', () => {
  // Moved from model.test.ts, where these two shapes covered `plannerLaneState`.
  // Both invariants are unchanged: a source that is still reading contributes
  // no rows and no total, and a source that reports no matching total gets
  // `null` rather than a stand-in zero.
  it('retains pending and unknown-total source states without inventing rows or zero totals', () => {
    expect(
      laneFromSourceProgress(
        'code',
        progress({
          phase: 'reading',
          outcome: 'pending',
          completed_units: null,
          total_units: null,
          page: null,
        }),
        [],
      ),
    ).toEqual({ state: 'pending', lane: 'code', phase: 'reading' });

    expect(
      laneFromSourceProgress(
        'knowledge',
        progress({
          source_id: 'knowledge',
          source_label: 'Knowledge',
          completed_units: 2,
          total_units: null,
          page: {
            offset: 0,
            limit: 25,
            total: null,
            next_offset: null,
            rows: [{ fact_id: KNOWLEDGE_FACT_ID, content: 'real fact' }],
            metadata: {},
          },
        }),
        [],
      ),
    ).toMatchObject({
      state: 'ready',
      lane: 'knowledge',
      reportedTotal: null,
      unreadableRows: 0,
      hits: [{ key: `knowledge:${KNOWLEDGE_FACT_ID}`, title: 'real fact' }],
    });
  });

  it('keeps unavailable, cancelled, error and ready-but-empty as four separate states', () => {
    const states = (['unavailable', 'cancelled', 'error'] as const).map(
      (outcome) =>
        laneFromSourceProgress(
          'code',
          progress({
            outcome,
            page: null,
            error_code: `${outcome}_code`,
            message: `${outcome} happened`,
          }),
          [],
        ).state,
    );
    const readyEmpty = laneFromSourceProgress('code', progress({}), []);

    expect(states).toEqual(['unavailable', 'cancelled', 'error']);
    expect(readyEmpty).toEqual({
      state: 'ready',
      lane: 'code',
      hits: [],
      reportedTotal: 0,
      unreadableRows: 0,
    });
    expect(new Set([...states, readyEmpty.state]).size).toBe(4);
  });

  it('carries the source error code and message rather than flattening them', () => {
    expect(
      laneFromSourceProgress(
        'knowledge',
        progress({
          source_id: 'knowledge',
          source_label: 'Knowledge',
          outcome: 'unavailable',
          page: null,
          error_code: 'fact_store_unavailable',
          message: 'the fact authority is not mounted',
        }),
        [],
      ),
    ).toEqual({
      state: 'unavailable',
      lane: 'knowledge',
      errorCode: 'fact_store_unavailable',
      detail: 'the fact authority is not mounted',
    });
  });

  it('treats a ready source that delivered no page as answered with nothing', () => {
    // Distinct from `unanswered`: the source did reply, and it replied empty.
    expect(laneFromSourceProgress('code', progress({ page: null }), [])).toEqual({
      state: 'ready',
      lane: 'code',
      hits: [],
      reportedTotal: null,
      unreadableRows: 0,
    });
  });

  it('reports rows it could not read instead of silently returning fewer', () => {
    const read = laneFromSourceProgress(
      'code',
      progress({
        page: {
          offset: 0,
          limit: 50,
          total: 3,
          next_offset: null,
          rows: [
            { id: 'n1', name: 'readable' },
            'not an object',
            // A keyed row carrying no identifier the grammar can title.
            { degree: 4 },
          ],
          metadata: {},
        },
      }),
      [],
    );

    expect(read).toMatchObject({ state: 'ready', unreadableRows: 2 });
    expect(laneHits(read)).toHaveLength(1);
  });

  it('leaves a field the row omitted absent rather than reading it as zero', () => {
    const read = laneFromSourceProgress(
      'code',
      progress({
        page: {
          offset: 0,
          limit: 50,
          total: 1,
          next_offset: null,
          rows: [{ id: 'n1', name: 'no_degree_here' }],
          metadata: {},
        },
      }),
      [],
    );
    const [hit] = laneHits(read);

    expect(hit?.title).toBe('no_degree_here');
    expect(hit?.signal).toBeUndefined();
    expect(hit?.stamp).toBeUndefined();
  });

  it('refuses to read a progress record addressed to another source', () => {
    expect(
      laneFromSourceProgress('sessions', progress({ source_id: 'code_graph' }), []),
    ).toEqual({ state: 'unanswered', lane: 'sessions' });
  });

  it('keeps an indexing semantic source apart from an unavailable one, chip and all', () => {
    const indexing = laneFromSourceProgress(
      'semantic',
      progress({
        source_id: 'semantic',
        source_label: 'Semantic',
        outcome: 'indexing',
        completed_units: 3,
        total_units: 10,
        page: null,
        error_code: 'semantic_indexing',
        message: 'semantic vector projection is in progress',
      }),
      [],
    );
    const unavailable = laneFromSourceProgress(
      'semantic',
      progress({
        source_id: 'semantic',
        source_label: 'Semantic',
        outcome: 'unavailable',
        page: null,
        error_code: 'semantic_runtime_unavailable',
        message: 'no mounted runtime',
      }),
      [],
    );

    expect(indexing).toEqual({
      state: 'indexing',
      lane: 'semantic',
      errorCode: 'semantic_indexing',
      detail: 'semantic vector projection is in progress',
      completedUnits: 3,
      totalUnits: 10,
    });
    expect(unavailable.state).toBe('unavailable');
    // Different chips: work in progress spins, a refusal does not.
    expect(laneStateKind(indexing)).toBe('loading');
    expect(laneStateKind(unavailable)).toBe('unavailable');
    expect(laneStateKind(indexing)).not.toBe(laneStateKind(unavailable));
    // The clause carries the stage and the provider's own progress accounting.
    expect(laneStateDetail(indexing)).toBe('semantic_indexing · 3/10');
  });

  it('reads an unactivated semantic source as a typed absence with the complete-zero chip', () => {
    const absent = laneFromSourceProgress(
      'semantic',
      progress({
        source_id: 'semantic',
        source_label: 'Semantic',
        outcome: 'absent',
        page: null,
        error_code: 'semantic_not_activated',
        message: 'semantic search is not activated for this project',
      }),
      [],
    );

    expect(absent).toEqual({
      state: 'absent',
      lane: 'semantic',
      errorCode: 'semantic_not_activated',
      detail: 'semantic search is not activated for this project',
    });
    expect(laneStateKind(absent)).toBe('complete_zero_findings');
    expect(laneStateKind(absent)).not.toBe('unavailable');
  });

  it('keeps stale, timed-out and unsupported as their own conditions and chips', () => {
    const reads = (['stale', 'timed_out', 'unsupported'] as const).map((outcome) =>
      laneFromSourceProgress(
        'code',
        progress({ outcome, page: null, error_code: `${outcome}_code`, message: null }),
        [],
      ),
    );

    expect(reads.map((read) => read.state)).toEqual(['stale', 'timed_out', 'unsupported']);
    expect(reads.map((read) => laneStateKind(read))).toEqual([
      'stale',
      'timed_out',
      'unsupported',
    ]);
  });

  it('counts partial rows while keeping the omission stated', () => {
    const partial = laneFromSourceProgress(
      'sessions',
      progress({
        source_id: 'sessions',
        source_label: 'Sessions',
        outcome: 'partial',
        error_code: 'lcm_temporal_read_incomplete',
        message: null,
        page: {
          offset: 0,
          limit: 50,
          total: null,
          next_offset: null,
          rows: [{ message_id: 'm1', content: 'partial row' }],
          metadata: {},
        },
      }),
      [],
    );

    expect(partial.state).toBe('partial');
    expect(laneHits(partial)).toHaveLength(1);
    expect(laneStateKind(partial)).toBe('partial');
    // Rows are genuine but the source itself said records were omitted, so
    // the count never presents as a measured denominator.
    expect(laneEvidence(partial)).toBe('associated');
  });
});

describe('searchLane', () => {
  it('keeps a client that cannot reach the daemon apart from an unavailable source', () => {
    const clientOffline = searchLane('code', { outcome: 'transport', state: 'offline' }, 'graph', []);
    const sourceUnavailable = searchLane(
      'code',
      answered(
        run({
          sources: [
            progress({
              outcome: 'unavailable',
              page: null,
              error_code: 'graph_index_unavailable',
              message: 'the code graph is not mounted',
            }),
          ],
        }),
      ),
      'graph',
      [],
    );

    expect(clientOffline).toEqual({ state: 'offline', lane: 'code' });
    expect(sourceUnavailable.state).toBe('unavailable');
    // Different chips, not merely different sentences: the taxonomy carries a
    // source-level unavailability state, so a reachable daemon reporting that
    // one source cannot serve is never drawn as a lost connection.
    expect(laneStateKind(clientOffline)).toBe('offline');
    expect(laneStateKind(sourceUnavailable)).toBe('unavailable');
    expect(laneStateKind(clientOffline)).not.toBe(laneStateKind(sourceUnavailable));
    // With the chip carrying the condition, the clause carries what the chip
    // cannot — the reason the source reported — and never repeats the state.
    expect(laneStateDetail(clientOffline)).toBe('daemon unreachable');
    expect(laneStateDetail(sourceUnavailable)).toBe('graph_index_unavailable');
  });

  it('falls back to the source message, then to no clause, for an unavailable lane', () => {
    const withMessageOnly = laneFromSourceProgress(
      'code',
      progress({
        outcome: 'unavailable',
        page: null,
        error_code: null,
        message: 'the code graph is not mounted',
      }),
      [],
    );
    const withNeither = laneFromSourceProgress(
      'code',
      progress({ outcome: 'unavailable', page: null, error_code: null, message: null }),
      [],
    );

    expect(laneStateDetail(withMessageOnly)).toBe('the code graph is not mounted');
    // A source that reported no reason gets no invented one; the chip alone
    // states the condition.
    expect(laneStateDetail(withNeither)).toBeUndefined();
    expect(laneStateKind(withNeither)).toBe('unavailable');
  });

  it('says a terminal run never named a source instead of showing it as reading', () => {
    expect(searchLane('sessions', answered(run({ state: 'completed' })), 'graph', [])).toEqual({
      state: 'unanswered',
      lane: 'sessions',
    });
    expect(searchLane('sessions', answered(run({ state: 'pending' })), 'graph', [])).toEqual({
      state: 'pending',
      lane: 'sessions',
      phase: null,
    });
  });

  it('does not attribute an earlier query\u2019s answer to the query on screen', () => {
    const earlier = answered(
      run({
        request: { query: 'other', limit: 50, offset: 0 },
        sources: [progress({})],
      }),
    );

    expect(searchLane('code', earlier, 'graph', [])).toEqual({
      state: 'pending',
      lane: 'code',
      phase: null,
    });
  });

  it('waits rather than guessing before any coordinator answer', () => {
    expect(searchLane('code', undefined, 'graph', [])).toEqual({
      state: 'pending',
      lane: 'code',
      phase: null,
    });
  });

  it('keeps every lane condition distinct end to end', () => {
    const conditions: ExplorerLaneReadModel[] = [
      searchLane('code', { outcome: 'transport', state: 'offline' }, 'graph', []),
      searchLane(
        'code',
        answered(run({ sources: [progress({ outcome: 'unavailable', page: null })] })),
        'graph',
        [],
      ),
      searchLane(
        'code',
        answered(run({ sources: [progress({ outcome: 'cancelled', page: null })] })),
        'graph',
        [],
      ),
      searchLane(
        'code',
        answered(run({ sources: [progress({ outcome: 'error', page: null })] })),
        'graph',
        [],
      ),
      searchLane('code', answered(run({ sources: [progress({})] })), 'graph', []),
      searchLane('code', answered(run({ state: 'completed' })), 'graph', []),
    ];

    expect(conditions.map((read) => read.state)).toEqual([
      'offline',
      'unavailable',
      'cancelled',
      'error',
      'ready',
      'unanswered',
    ]);
    expect(new Set(conditions.map((read) => read.state)).size).toBe(6);
    // Only the source that answered may contribute rows or a count.
    expect(conditions.filter((read) => read.state === 'ready')).toHaveLength(1);
  });
});

describe('run state', () => {
  it('keeps a timed-out coordinator run terminal and visibly timed out', () => {
    expect(runIsTerminal('timed_out')).toBe(true);
    expect(runStateKind('timed_out')).toBe('timed_out');
  });
});

describe('laneFromTransport', () => {
  it('keeps an authorization refusal apart from a broken read', () => {
    expect(laneFromTransport('code', 'unauthorized', null).state).toBe('unauthorized');
    expect(laneFromTransport('code', 'denied', null).state).toBe('denied');
    expect(laneFromTransport('code', 'unsupported_schema', null).state).toBe(
      'unsupported_schema',
    );
    expect(laneFromTransport('code', 'error', 'HTTP 500')).toEqual({
      state: 'error',
      lane: 'code',
      errorCode: null,
      detail: 'HTTP 500',
    });
  });

  it('carries a domain state it has no lane reading for rather than calling it an error', () => {
    expect(laneFromTransport('code', 'stale', null)).toEqual({
      state: 'indeterminate',
      lane: 'code',
      domainState: 'stale',
    });
    expect(laneStateDetail(laneFromTransport('code', 'stale', null))).toBe('stale');
  });
});

describe('browseLane', () => {
  it('reports an overview that answered without claiming a matching total', () => {
    const read = browseLane(
      'code',
      browseAnswered([{ id: 'n1', name: 'browse_row' }]),
      false,
      (rows: readonly Record<string, unknown>[]) => rows,
      [],
    );

    expect(read).toMatchObject({ state: 'ready', reportedTotal: null, unreadableRows: 0 });
    expect(laneHits(read)).toHaveLength(1);
  });

  it('preserves each canonical transport reading as its own state', () => {
    const rowsOf = (rows: readonly Record<string, unknown>[]) => rows;
    const states = (['offline', 'unauthorized', 'denied', 'unsupported_schema'] as const).map(
      (state) => browseLane('code', { outcome: 'transport', state }, false, rowsOf, []).state,
    );

    expect(states).toEqual(['offline', 'unauthorized', 'denied', 'unsupported_schema']);
  });
});

describe('laneEvidence', () => {
  it('reserves measured for a source that reported a real denominator', () => {
    const withTotal = laneFromSourceProgress(
      'code',
      progress({
        page: {
          offset: 0,
          limit: 50,
          total: 12,
          next_offset: null,
          rows: [{ id: 'n1', name: 'row' }],
          metadata: {},
        },
      }),
      [],
    );
    const withoutTotal = laneFromSourceProgress(
      'code',
      progress({
        page: {
          offset: 0,
          limit: 50,
          total: null,
          next_offset: null,
          rows: [{ id: 'n1', name: 'row' }],
          metadata: {},
        },
      }),
      [],
    );

    expect(laneEvidence(withTotal)).toBe('measured');
    expect(laneEvidence(withoutTotal)).toBe('associated');
    expect(laneEvidence(laneFromTransport('code', 'offline', null))).toBe('unknown');
    expect(laneEvidence(undefined)).toBe('unknown');
  });
});

describe('laneHits', () => {
  it('yields nothing for every lane that did not answer', () => {
    const notReady: ExplorerLaneReadModel[] = [
      { state: 'pending', lane: 'code', phase: null },
      { state: 'unavailable', lane: 'code', errorCode: null, detail: null },
      { state: 'cancelled', lane: 'code', errorCode: null, detail: null },
      { state: 'error', lane: 'code', errorCode: null, detail: null },
      { state: 'offline', lane: 'code' },
      { state: 'unanswered', lane: 'code' },
    ];

    for (const read of notReady) {
      expect(laneHits(read)).toEqual([]);
    }
  });
});
