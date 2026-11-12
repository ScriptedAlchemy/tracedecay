import { describe, expect, it } from 'vitest';
import {
  ExplorerSourceProgressV1Schema,
  ExplorerStreamEventV1Schema,
  type DashboardEnvelopeV1,
  type ExplorerSourceProgressV1,
} from '../../contracts/generated.ts';
import type { EnvelopeResult } from '../../data/query/envelope.ts';
import {
  browseLane,
  laneAnswered,
  laneEvidence,
  laneFromScope,
  laneFromSourceProgress,
  laneFromTransport,
  laneHits,
  lanePending,
  laneStateDetail,
  laneStateKind,
  searchIsTerminal,
  searchLane,
  searchStateKind,
  SEMANTIC_UNREGISTERED_DETAIL,
  semanticLane,
  startedSearch,
  withStreamEvent,
  type ExplorerLaneReadModel,
  type ExplorerSearchResult,
  type ExplorerSearchStatus,
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
    page: { limit: 50, total: 0, rows: [], metadata: {} },
    ...over,
  });
}

/** A search for `graph` that has published `sources` and stands at `status`. */
function searching(
  sources: ExplorerSourceProgressV1[] = [],
  status: ExplorerSearchStatus = 'streaming',
  query = 'graph',
): ExplorerSearchResult {
  return { outcome: 'search', search: { ...startedSearch(query), sources, status } };
}

function browseAnswered<T>(payload: T): EnvelopeResult<T> {
  const envelope = {
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
      matched: null,
      excluded: null,
      omitted: 0,
      unknown: null,
      denominator: 1,
      unit: 'rows',
      omission_reasons: [],
    },
    freshness: { state: 'unknown', observed_at_micros: null, watermark: null },
    domain_state: 'ready',
    legal_actions: [],
    payload,
  };
  return { outcome: 'envelope', envelope: envelope as unknown as DashboardEnvelopeV1<T> };
}

describe('laneFromSourceProgress', () => {
  // Both invariants are unchanged from the planner era: a source that
  // reports no matching total gets `null` rather than a stand-in zero, and a
  // source publishes only once it has concluded, so there is no pending
  // source record to read.
  it('retains an unknown-total source state without inventing rows or zero totals', () => {
    expect(
      laneFromSourceProgress(
        'knowledge',
        progress({
          source_id: 'knowledge',
          source_label: 'Knowledge',
          completed_units: 2,
          total_units: null,
          page: {
            limit: 25,
            total: null,
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
      freshness: 'unknown',
      watermark: null,
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
      freshness: 'unknown',
      watermark: null,
    });
  });

  it('carries the source\u2019s own freshness word and watermark verbatim', () => {
    const fresh = laneFromSourceProgress(
      'code',
      progress({ freshness: 'fresh', watermark: 'g-42' }),
      [],
    );
    expect(fresh).toMatchObject({ state: 'ready', freshness: 'fresh', watermark: 'g-42' });
  });

  it('reports rows it could not read instead of silently returning fewer', () => {
    const read = laneFromSourceProgress(
      'code',
      progress({
        page: {
          limit: 50,
          total: 3,
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
          limit: 50,
          total: 1,
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

  it('keeps stale and timed-out as their own conditions and chips', () => {
    const reads = (['stale', 'timed_out'] as const).map((outcome) =>
      laneFromSourceProgress(
        'code',
        progress({ outcome, page: null, error_code: `${outcome}_code`, message: null }),
        [],
      ),
    );

    expect(reads.map((read) => read.state)).toEqual(['stale', 'timed_out']);
    expect(reads.map((read) => laneStateKind(read))).toEqual(['stale', 'timed_out']);
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
          limit: 50,
          total: null,
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
      searching([
        progress({
          outcome: 'unavailable',
          page: null,
          error_code: 'graph_index_unavailable',
          message: 'the code graph is not mounted',
        }),
      ]),
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
    // cannot, the reason the source reported, and never repeats the state.
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

  it('says a concluded search never published a source instead of showing it as reading', () => {
    expect(searchLane('sessions', searching([], 'completed'), 'graph', [])).toEqual({
      state: 'unanswered',
      lane: 'sessions',
    });
    expect(searchLane('sessions', searching([], 'streaming'), 'graph', [])).toEqual({
      state: 'pending',
      lane: 'sessions',
    });
  });

  it('settles an unpublished lane by how the search ended', () => {
    expect(searchLane('sessions', searching([], 'cancelled'), 'graph', [])).toMatchObject({
      state: 'cancelled',
      errorCode: 'request_cancelled',
    });
    expect(searchLane('sessions', searching([], 'interrupted'), 'graph', [])).toEqual({
      state: 'offline',
      lane: 'sessions',
    });
    // A source that did publish is never overwritten by how the search ended.
    expect(
      searchLane('code', searching([progress({})], 'cancelled'), 'graph', []).state,
    ).toBe('ready');
  });

  it('does not attribute an earlier query\u2019s answer to the query on screen', () => {
    const earlier = searching([progress({})], 'completed', 'other');

    expect(searchLane('code', earlier, 'graph', [])).toEqual({ state: 'pending', lane: 'code' });
  });

  it('waits rather than guessing before any search answer', () => {
    expect(searchLane('code', undefined, 'graph', [])).toEqual({ state: 'pending', lane: 'code' });
  });

  it('keeps every lane condition distinct end to end', () => {
    const conditions: ExplorerLaneReadModel[] = [
      searchLane('code', { outcome: 'transport', state: 'offline' }, 'graph', []),
      searchLane('code', searching([progress({ outcome: 'unavailable', page: null })]), 'graph', []),
      searchLane('code', searching([progress({ outcome: 'cancelled', page: null })]), 'graph', []),
      searchLane('code', searching([progress({ outcome: 'error', page: null })]), 'graph', []),
      searchLane('code', searching([progress({})]), 'graph', []),
      searchLane('code', searching([], 'completed'), 'graph', []),
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

describe('search stream', () => {
  const event = (frame: unknown) => ExplorerStreamEventV1Schema.parse(frame);

  it('folds each source as it arrives and the final frame once', () => {
    const code = progress({});
    const sessions = progress({ source_id: 'sessions', source_label: 'Sessions' });
    let search = startedSearch('graph');
    search = withStreamEvent(search, event({ event: 'source', source: code }));
    expect(search.sources.map((source) => source.source_id)).toEqual(['code_graph']);
    expect(search.status).toBe('streaming');
    expect(searchIsTerminal(search.status)).toBe(false);

    search = withStreamEvent(search, event({ event: 'source', source: sessions }));
    search = withStreamEvent(
      search,
      event({ event: 'final', outcome: 'partial', elapsed_micros: 17 }),
    );
    expect(search.sources.map((source) => source.source_id)).toEqual(['code_graph', 'sessions']);
    expect(search).toMatchObject({ status: 'partial', elapsedMicros: 17 });
    expect(searchIsTerminal(search.status)).toBe(true);
  });

  it('replaces a repeated source frame instead of double-counting the lane', () => {
    let search = startedSearch('graph');
    for (const outcome of ['stale', 'ready'] as const) {
      search = withStreamEvent(
        search,
        event({ event: 'source', source: progress({ outcome, page: null }) }),
      );
    }
    expect(search.sources).toHaveLength(1);
    expect(search.sources[0]?.outcome).toBe('ready');
  });

  it('keeps a timed-out search terminal and visibly timed out', () => {
    expect(searchIsTerminal('timed_out')).toBe(true);
    expect(searchStateKind('timed_out')).toBe('timed_out');
    expect(searchStateKind('streaming')).toBe('loading');
    expect(searchStateKind('interrupted')).toBe('offline');
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
      detail: null,
    });
    expect(laneStateDetail(laneFromTransport('code', 'stale', null))).toBe('stale');
  });

  it('keeps a read-only scope refusal as locked, carrying the gateway sentence', () => {
    const locked = laneFromTransport('sessions', 'locked', 'read-only for non-active projects');
    expect(locked).toEqual({
      state: 'locked',
      lane: 'sessions',
      detail: 'read-only for non-active projects',
    });
    expect(laneStateKind(locked)).toBe('locked');
    expect(laneStateDetail(locked)).toBe('read-only for non-active projects');
  });
});

describe('semanticLane', () => {
  it('is a standing typed absence, not an unavailable source and not an empty answer', () => {
    const lane = semanticLane();
    expect(lane.state).toBe('unregistered');
    expect(lane.lane).toBe('semantic');
    // Rendered with the taxonomy's unavailable chip, but the detail names the
    // contract fact: there is no source to have declined.
    expect(laneStateKind(lane)).toBe('unavailable');
    expect(laneStateDetail(lane)).toBe(SEMANTIC_UNREGISTERED_DETAIL);
    expect(laneHits(lane)).toEqual([]);
    expect(laneAnswered(lane)).toBe(false);
    expect(lanePending(lane)).toBe(false);
    expect(laneEvidence(lane)).toBe('unknown');
  });
});

describe('laneFromScope', () => {
  it('lets a writable scope proceed and refuses the other two with their own reasons', () => {
    expect(laneFromScope('code', { state: 'writable', target: 'the active project' })).toBeNull();
    expect(
      laneFromScope('code', { state: 'read_only', reason: 'Other is not the active project.' }),
    ).toEqual({ state: 'locked', lane: 'code', detail: 'Other is not the active project.' });
    const unknown = laneFromScope('knowledge', {
      state: 'unknown',
      reason: 'not checked against the registry yet',
    });
    expect(unknown).toEqual({
      state: 'indeterminate',
      lane: 'knowledge',
      domainState: 'unknown',
      detail: 'not checked against the registry yet',
    });
    expect(laneStateKind(unknown!)).toBe('unknown');
    expect(laneStateDetail(unknown!)).toBe('not checked against the registry yet');
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
          limit: 50,
          total: 12,
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
          limit: 50,
          total: null,
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
      { state: 'pending', lane: 'code' },
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
