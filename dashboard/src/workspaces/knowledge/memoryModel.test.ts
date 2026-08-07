/**
 * The memory readings, proved against the shapes their handlers actually emit.
 *
 * Every case here is a lie the naive rendering of one of these payloads would
 * have told. The model exists to make each of them a compile-and-test-time
 * concern rather than something a reader has to notice on screen.
 */
import { describe, expect, it } from 'vitest';

import type {
  CurationRunsPayload,
  OplogPayload,
  ProjectionPayload,
  SimilarityPayload,
  TrustHistoryPayload,
} from '../../data/query/memory.ts';
import {
  curationRunsReading,
  oplogDetailReading,
  oplogReading,
  projectionReading,
  runStatusState,
  similarityReading,
  trustDetailState,
  trustHistoryReading,
} from './memoryModel.ts';

/* ---- trust history ------------------------------------------------------- */

function trustEvent(overrides: Partial<TrustHistoryPayload['trust_history'][number]> = {}) {
  return {
    timestamp: '2026-08-01T00:00:00Z',
    action: 'helpful' as const,
    old_trust: 0.5,
    new_trust: 0.6,
    delta: 0.1,
    details_availability: 'available' as const,
    ...overrides,
  };
}

function trustPayload(
  events: TrustHistoryPayload['trust_history'],
  repair: TrustHistoryPayload['repair'],
): TrustHistoryPayload {
  return { fact_id: 7, trust_history: events, repair, error: '' };
}

describe('trustHistoryReading', () => {
  it('nets the opening and closing trust across the appended events', () => {
    const reading = trustHistoryReading(
      trustPayload(
        [
          trustEvent({ old_trust: 0.5, new_trust: 0.6, delta: 0.1 }),
          trustEvent({
            action: 'unhelpful',
            old_trust: 0.6,
            new_trust: 0.45,
            delta: -0.15,
          }),
        ],
        { state: 'not_required', processed: null, remaining: null },
      ),
    );
    expect(reading.count).toBe(2);
    expect(reading.helpful).toBe(1);
    expect(reading.unhelpful).toBe(1);
    expect(reading.opening).toBe(0.5);
    expect(reading.closing).toBe(0.45);
    expect(reading.net).toBeCloseTo(-0.05, 10);
  });

  it('reports no opening, closing or net for an audit with no events', () => {
    const reading = trustHistoryReading(
      trustPayload([], { state: 'not_required', processed: null, remaining: null }),
    );
    // Not zero. A fact nothing has ever rated has no measured movement, and a
    // `0.000` net would claim feedback arrived and cancelled out.
    expect(reading.opening).toBeNull();
    expect(reading.closing).toBeNull();
    expect(reading.net).toBeNull();
  });

  it('counts every detail-availability tier, zeroes included', () => {
    const reading = trustHistoryReading(
      trustPayload(
        [
          trustEvent({ details_availability: 'legacy_redacted' }),
          trustEvent({ details_availability: 'unknown' }),
          trustEvent({ details_availability: 'unknown' }),
        ],
        { state: 'complete', processed: 12, remaining: 0 },
      ),
    );
    // The zero is as load-bearing as the counts: "0 of 3 available" is what
    // lets the panel say how much of the audit it can actually show.
    expect(reading.availability).toEqual({
      available: 0,
      legacy_redacted: 1,
      unknown: 2,
    });
  });

  it('never reports an unknown repair state as a complete audit', () => {
    const unknown = trustHistoryReading(
      trustPayload([], { state: 'unknown', processed: null, remaining: null }),
    );
    expect(unknown.repair).toMatch(/cannot say whether/);
    expect(unknown.repair).toMatch(/may be incomplete/);
  });

  it('states an unfinished repair with its remaining count, and without one', () => {
    expect(
      trustHistoryReading(
        trustPayload([], { state: 'incomplete', processed: 4, remaining: 96 }),
      ).repair,
    ).toMatch(/96 rows to go/);
    expect(
      trustHistoryReading(
        trustPayload([], { state: 'incomplete', processed: null, remaining: null }),
      ).repair,
    ).toMatch(/did not report how much is left/);
  });
});

describe('trustDetailState', () => {
  it('keeps a withheld detail apart from an unrecorded one', () => {
    expect(trustDetailState('available')).toBeNull();
    expect(trustDetailState('legacy_redacted')).toBe('redacted');
    expect(trustDetailState('unknown')).toBe('unknown');
  });
});

/* ---- projection ---------------------------------------------------------- */

function point(overrides: Partial<ProjectionPayload['points'][number]> = {}) {
  return {
    fact_id: 1,
    x: 0,
    y: 0,
    category: 'general',
    content: 'a fact',
    trust_score: 0.5,
    retrieval_count: 0,
    created_at: 0,
    updated_at: 0,
    bank_name: null,
    entity_count: 0,
    connection_count: 0,
    ...overrides,
  };
}

function projectionPayload(overrides: Partial<ProjectionPayload> = {}): ProjectionPayload {
  return {
    exists: true,
    dim: 64,
    limit: 400,
    method: 'pca',
    points: [],
    error: '',
    ...overrides,
  };
}

describe('projectionReading', () => {
  it('treats a pca decomposition over two or more points as a projection', () => {
    const reading = projectionReading(
      projectionPayload({
        points: [
          point({ fact_id: 1, x: -1.5, y: 0.25, category: 'decision' }),
          point({ fact_id: 2, x: 2, y: -1, category: 'decision' }),
          point({ fact_id: 3, x: 0.5, y: 3, category: 'code_area' }),
        ],
      }),
    );
    expect(reading.projected).toBe(true);
    expect(reading.extent).toEqual({ x: [-1.5, 2], y: [-1, 3] });
    // Ranked by population so the legend is an ordering rather than emission
    // order; ties break by name so the render is deterministic.
    expect(reading.categories).toEqual([
      { category: 'decision', count: 2 },
      { category: 'code_area', count: 1 },
    ]);
    expect(reading.note).toMatch(/principal components of 3 phase vectors/);
  });

  it('refuses to call a `none` method a projection even when it returned points', () => {
    const reading = projectionReading(
      projectionPayload({ method: 'none', dim: 64, points: [point()] }),
    );
    // The handler emits a single point at the origin for a store with one
    // vectored fact. Drawn as a scatter it is indistinguishable from a real
    // projection with one tight cluster.
    expect(reading.projected).toBe(false);
    expect(reading.note).toMatch(/placeholders, not a projection/);
  });

  it('separates a store with no vectors from one that could not be decomposed', () => {
    const empty = projectionReading(projectionPayload({ method: 'none', dim: 0, points: [] }));
    expect(empty.projected).toBe(false);
    expect(empty.note).toMatch(/no fact in this store carries a phase vector/);
    expect(empty.extent).toBeNull();
  });

  it('does not call a two-point pca result unprojected', () => {
    const reading = projectionReading(
      projectionPayload({ points: [point({ fact_id: 1 }), point({ fact_id: 2, x: 1 })] }),
    );
    expect(reading.projected).toBe(true);
  });
});

/* ---- similarity ---------------------------------------------------------- */

function similarityPayload(overrides: Partial<SimilarityPayload> = {}): SimilarityPayload {
  return {
    exists: true,
    dim: 64,
    count: 40,
    limit: 25,
    min_similarity: 0.85,
    total_pairs: 300,
    score_distribution: {
      min_score: 0.1,
      max_score: 0.99,
      average_score: 0.42,
      bin_count: 10,
      total_pairs: 300,
      bins: [],
    },
    pairs: [],
    error: '',
    ...overrides,
  };
}

function pair(a: number, b: number, similarity: number) {
  return {
    a_id: a,
    b_id: b,
    a_content: `fact ${a}`,
    b_content: `fact ${b}`,
    a_category: 'general',
    b_category: 'general',
    similarity,
    classification: 'likely_duplicate',
  };
}

describe('similarityReading', () => {
  it('keeps the three denominators apart in one sentence', () => {
    const reading = similarityReading(
      similarityPayload({ pairs: [pair(1, 2, 0.97), pair(3, 4, 0.9)] }),
    );
    expect(reading.vectored).toBe(40);
    expect(reading.scored).toBe(300);
    expect(reading.returned).toBe(2);
    expect(reading.denominators).toBe(
      '2 shown of 300 scored pairs over 40 vectored facts, at or above 0.85',
    );
  });

  it('marks a return truncated by the cap rather than by the floor', () => {
    const capped = similarityReading(
      similarityPayload({
        limit: 2,
        total_pairs: 9,
        pairs: [pair(1, 2, 0.97), pair(3, 4, 0.96)],
      }),
    );
    expect(capped.capped).toBe(true);
    const complete = similarityReading(
      similarityPayload({ limit: 25, total_pairs: 2, pairs: [pair(1, 2, 0.97), pair(3, 4, 0.9)] }),
    );
    expect(complete.capped).toBe(false);
  });

  it('says a pair needs two facts rather than reporting zero similarity', () => {
    const reading = similarityReading(
      similarityPayload({
        count: 1,
        total_pairs: 0,
        pairs: [],
        score_distribution: {
          min_score: null,
          max_score: null,
          average_score: null,
          bin_count: 0,
          total_pairs: 0,
          bins: [],
        },
      }),
    );
    // Every statistic stays null. `0.0000` here would be a measured mean
    // similarity of zero, which is a different and false claim.
    expect(reading.average).toBeNull();
    expect(reading.min).toBeNull();
    expect(reading.max).toBeNull();
    expect(reading.denominators).toBe('1 vectored fact — a pair needs two, so nothing was scored');
  });
});

/* ---- curation runs ------------------------------------------------------- */

function run(overrides: Partial<CurationRunsPayload['records'][number]> = {}) {
  return {
    run_id: 'run-1',
    trigger: 'scheduler',
    task: 'memory_curator',
    backend: 'codex_app_server',
    status: 'completed',
    reviewed_count: 3,
    accepted_count: 2,
    rejected_count: 1,
    skipped_count: 0,
    started_at: '2026-08-01T00:00:00Z',
    completed_at: '2026-08-01T00:01:00Z',
    ...overrides,
  };
}

describe('curationRunsReading', () => {
  it('groups runs by task and counts the ones the ledger did not record as completed', () => {
    const reading = curationRunsReading({
      records: [
        run({ run_id: 'a' }),
        run({ run_id: 'b', accepted_count: 4, rejected_count: 0, skipped_count: 2 }),
        run({ run_id: 'c', task: 'skill_writer', status: 'failed', error: 'backend timeout' }),
      ],
      count: 3,
      limit: 50,
      error: '',
    });
    expect(reading.failed).toBe(1);
    expect(reading.tasks).toEqual([
      { task: 'memory_curator', runs: 2, accepted: 6, rejected: 1, skipped: 2, failed: 0 },
      // The failed run's own counts still total: the ledger recorded what it
      // reviewed before it failed, and dropping those would understate the
      // work the task actually did.
      { task: 'skill_writer', runs: 1, accepted: 2, rejected: 1, skipped: 0, failed: 1 },
    ]);
  });

  it('separates an unreadable ledger from a project that has never run automation', () => {
    // Both answer HTTP 200 with an empty array. Only `error` tells them apart,
    // and a surface that read `records` first would call the first one empty.
    const unreadable = curationRunsReading({
      records: [],
      count: 0,
      limit: 50,
      error: 'ledger is corrupt at line 12',
    });
    expect(unreadable.ledgerError).toBe('ledger is corrupt at line 12');
    const never = curationRunsReading({ records: [], count: 0, limit: 50, error: '' });
    expect(never.ledgerError).toBeNull();
  });

  it('counts a run failed on its recorded status, not on carrying an error string', () => {
    const reading = curationRunsReading({
      records: [run({ status: 'completed', error: 'one proposal was malformed' })],
      count: 1,
      limit: 50,
      error: '',
    });
    expect(reading.failed).toBe(0);
  });
});

describe('runStatusState', () => {
  it('maps the ledger vocabulary onto the state taxonomy', () => {
    expect(runStatusState('completed')).toBe('ready');
    expect(runStatusState('failed')).toBe('error');
    expect(runStatusState('cancelled')).toBe('cancelled');
    expect(runStatusState('timed_out')).toBe('timed_out');
  });

  it('leaves a status this build has not seen as unknown rather than as an error', () => {
    // Inventing a verdict for the daemon's vocabulary is the fabrication the
    // taxonomy exists to prevent — an unrecognised word is not a failure.
    expect(runStatusState('quiesced')).toBe('unknown');
  });
});

/* ---- oplog --------------------------------------------------------------- */

describe('oplogDetailReading', () => {
  it('keeps the three detail variants apart', () => {
    expect(oplogDetailReading({ summary: 'stored a fact' })).toEqual({
      kind: 'summary',
      summary: 'stored a fact',
    });
    const redacted = oplogDetailReading({ redacted: true });
    expect(redacted).toMatchObject({ kind: 'state', state: 'redacted' });
    const unknown = oplogDetailReading({ availability: 'unknown' });
    expect(unknown).toMatchObject({ kind: 'state', state: 'unknown' });
    // The whole point: a withheld detail and an unrecorded one are not the
    // same reading, and neither is "no detail".
    expect(redacted).not.toEqual(unknown);
  });
});

function oplogPayload(overrides: Partial<OplogPayload> = {}): OplogPayload {
  return { events: [], count: 0, limit: 100, error: '', ...overrides };
}

describe('oplogReading', () => {
  it('tallies operations and the two withheld-detail tiers', () => {
    const reading = oplogReading(
      oplogPayload({
        events: [
          { id: 1, ts: 't1', op: 'add_fact', fact_id: 7, detail: { summary: 'added' } },
          { id: 2, ts: 't2', op: 'add_fact', fact_id: 8, detail: { redacted: true } },
          { id: 3, ts: 't3', op: 'remove_fact', fact_id: null, detail: { availability: 'unknown' } },
        ],
        count: 3,
      }),
    );
    expect(reading.operations).toEqual([
      { op: 'add_fact', count: 2 },
      { op: 'remove_fact', count: 1 },
    ]);
    expect(reading.redacted).toBe(1);
    expect(reading.unknownDetail).toBe(1);
  });

  it('separates an unreadable store from a store nothing has written to', () => {
    expect(oplogReading(oplogPayload({ error: 'database is locked' })).storeError).toBe(
      'database is locked',
    );
    expect(oplogReading(oplogPayload()).storeError).toBeNull();
  });
});
