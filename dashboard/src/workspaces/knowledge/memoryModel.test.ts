/**
 * The memory readings, proved against the shapes their handlers actually emit.
 *
 * Every case here is a lie the naive rendering of one of these payloads would
 * have told. The model exists to make each of them a compile-and-test-time
 * concern rather than something a reader has to notice on screen.
 */
import { describe, expect, it } from 'vitest';

import type {
  OplogPayload,
  ProjectionPayload,
  SimilarityPayload,
  TrustHistoryPayload,
} from '../../data/query/memory.ts';
import {
  OplogPayloadSchema,
  TrustHistoryPayloadSchema,
} from '../../data/query/memory.ts';
import {
  formatUtcMicros,
  oplogReading,
  projectionReading,
  similarityReading,
  trustDetailState,
  trustHistoryReading,
} from './memoryModel.ts';

/* ---- trust history ------------------------------------------------------- */

describe('formatUtcMicros', () => {
  it('formats canonical microseconds for presentation without changing the wire value', () => {
    expect(formatUtcMicros(1_754_006_400_000_000)).toBe('2025-08-01T00:00:00.000Z');
  });
});

function trustEvent(overrides: Partial<TrustHistoryPayload['trust_history'][number]> = {}) {
  return {
    event_id: 'event-project-1',
    timestamp: 1_754_006_400_000_000,
    action: 'helpful' as const,
    old_trust: 0.5,
    new_trust: 0.6,
    delta: 0.1,
    details_availability: 'available' as const,
    ...overrides,
  };
}

function trustPayload(events: TrustHistoryPayload['trust_history']): TrustHistoryPayload {
  return {
    fact_id: 'fact-project-7',
    trust_history: events,
    limit: 300,
    completeness: 'complete',
    next_after: null,
    error: '',
  };
}

describe('trustHistoryReading', () => {
  it('accepts canonical microseconds and rejects display timestamps on the wire', () => {
    const canonical = trustPayload([trustEvent()]);
    expect(TrustHistoryPayloadSchema.safeParse(canonical).success).toBe(true);
    expect(
      TrustHistoryPayloadSchema.safeParse({
        ...canonical,
        trust_history: [{ ...canonical.trust_history[0], timestamp: '2026-08-01T00:00:00Z' }],
      }).success,
    ).toBe(false);
    expect(
      TrustHistoryPayloadSchema.safeParse({
        ...canonical,
        trust_history: [{ ...canonical.trust_history[0], timestamp: 1.5 }],
      }).success,
    ).toBe(false);
  });

  it('requires partial history to carry its exact continuation', () => {
    const canonical = trustPayload([trustEvent()]);
    expect(
      TrustHistoryPayloadSchema.safeParse({
        ...canonical,
        completeness: 'partial',
        next_after: {
          occurred_at: canonical.trust_history[0]!.timestamp,
          event_id: canonical.trust_history[0]!.event_id,
        },
      }).success,
    ).toBe(true);
    expect(
      TrustHistoryPayloadSchema.safeParse({
        ...canonical,
        completeness: 'partial',
        next_after: null,
      }).success,
    ).toBe(false);
  });

  it('nets the opening and closing trust across the appended events', () => {
    const reading = trustHistoryReading(
      trustPayload([
        trustEvent({ old_trust: 0.5, new_trust: 0.6, delta: 0.1 }),
        trustEvent({
          action: 'unhelpful',
          old_trust: 0.6,
          new_trust: 0.45,
          delta: -0.15,
        }),
      ]),
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
      trustPayload([]),
    );
    // Not zero. A returned window with no rows has no measured movement, and a
    // `0.000` net would claim feedback arrived and cancelled out.
    expect(reading.opening).toBeNull();
    expect(reading.closing).toBeNull();
    expect(reading.net).toBeNull();
  });

  it('counts every detail-availability tier, zeroes included', () => {
    const reading = trustHistoryReading(
      trustPayload([
        trustEvent({ details_availability: 'redacted' }),
        trustEvent({ details_availability: 'unknown' }),
        trustEvent({ details_availability: 'unknown' }),
      ]),
    );
    // The zero is as load-bearing as the counts: "0 of 3 available" is what
    // lets the panel say how much of the audit it can actually show.
    expect(reading.availability).toEqual({
      available: 0,
      redacted: 1,
      unknown: 2,
    });
  });
});

describe('trustDetailState', () => {
  it('keeps a withheld detail apart from an unrecorded one', () => {
    expect(trustDetailState('available')).toBeNull();
    expect(trustDetailState('redacted')).toBe('redacted');
    expect(trustDetailState('unknown')).toBe('unknown');
  });
});

/* ---- projection ---------------------------------------------------------- */

function point(overrides: Partial<ProjectionPayload['points'][number]> = {}) {
  return {
    fact_id: 'fact-project-1',
    payload_access: 'eligible' as const,
    x: 0,
    y: 0,
    category: 'general',
    content: 'a fact',
    trust_score: 0.5,
    retrieval_count: 0,
    access_count: 0,
    helpful_count: 0,
    unhelpful_count: 0,
    created_at: 1_754_006_400_000_000,
    updated_at: 1_754_006_400_000_000,
    projected_as_of: 1_754_006_400_000_000,
    last_recalled_at: null,
    tags: [],
    entities: [],
    metadata: {},
    entity_count: 0,
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
    coverage: {
      completeness: 'complete',
      examined: 0,
      limit: 400,
      omission_reasons: [],
    },
    error: '',
    ...overrides,
  };
}

describe('projectionReading', () => {
  it('treats a pca decomposition over two or more points as a projection', () => {
    const reading = projectionReading(
      projectionPayload({
        points: [
          point({ fact_id: 'fact-project-1', x: -1.5, y: 0.25, category: 'decision' }),
          point({ fact_id: 'fact-project-2', x: 2, y: -1, category: 'decision' }),
          point({ fact_id: 'fact-project-3', x: 0.5, y: 3, category: 'code_area' }),
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
    expect(reading.note).toMatch(/principal components of 3 query-time-derived phase encodings returned by a request bounded to 400 facts/);
  });

  it('refuses to call a `none` method a projection even when it returned points', () => {
    const reading = projectionReading(
      projectionPayload({ method: 'none', dim: 64, points: [point()] }),
    );
    // The handler emits a single point at the origin for a store with one
    // query-time encoded fact. Drawn as a scatter it is indistinguishable from a real
    // projection with one tight cluster.
    expect(reading.projected).toBe(false);
    expect(reading.note).toMatch(/placeholders, not a projection/);
  });

  it('separates a store with no vectors from one that could not be decomposed', () => {
    const empty = projectionReading(projectionPayload({
      method: 'none',
      dim: 0,
      points: [],
      coverage: {
        completeness: 'bounded',
        examined: 400,
        limit: 400,
        omission_reasons: ['request_limit_reached'],
      },
    }));
    expect(empty.projected).toBe(false);
    expect(empty.note).toMatch(/returned no phase encodings; whole-store coverage is unknown/);
    expect(empty.extent).toBeNull();
  });

  it('does not call a two-point pca result unprojected', () => {
    const reading = projectionReading(
      projectionPayload({
        points: [point({ fact_id: 'fact-project-1' }), point({ fact_id: 'fact-project-2', x: 1 })],
      }),
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

function pair(a: string, b: string, similarity: number) {
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
      similarityPayload({
        pairs: [
          pair('fact-project-1', 'fact-project-2', 0.97),
          pair('fact-project-3', 'fact-project-4', 0.9),
        ],
      }),
    );
    expect(reading.encoded).toBe(40);
    expect(reading.scored).toBe(300);
    expect(reading.returned).toBe(2);
    expect(reading.denominators).toBe(
      '2 pairs shown at or above 0.85; 300 finite pairs scored globally over 40 query-time encoded facts',
    );
  });

  it('does not infer cap truncation from the pre-floor global pair count', () => {
    const fullPageWithUnknownCoverage = similarityReading(
      similarityPayload({
        limit: 2,
        total_pairs: 9,
        score_distribution: {
          min_score: 0.1,
          max_score: 0.97,
          average_score: 0.37,
          bin_count: 2,
          total_pairs: 9,
          bins: [
            { start: 0.1, end: 0.84, count: 7 },
            { start: 0.85, end: 0.97, count: 2 },
          ],
        },
        pairs: [
          pair('fact-project-1', 'fact-project-2', 0.97),
          pair('fact-project-3', 'fact-project-4', 0.96),
        ],
      }),
    );
    // Exactly two pairs meet the threshold and seven more fall below it. The
    // response fills its limit, but `total_pairs` counts all nine finite pairs
    // before the threshold, so it cannot prove a third threshold match exists.
    expect(fullPageWithUnknownCoverage.capped).toBeNull();

    const underLimit = similarityReading(
      similarityPayload({
        limit: 25,
        total_pairs: 2,
        pairs: [
          pair('fact-project-1', 'fact-project-2', 0.97),
          pair('fact-project-3', 'fact-project-4', 0.9),
        ],
      }),
    );
    expect(underLimit.capped).toBe(false);
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
    expect(reading.denominators).toBe(
      '1 query-time encoded fact — a pair needs two, so nothing was scored',
    );
  });
});

/* ---- oplog --------------------------------------------------------------- */

function oplogPayload(overrides: Partial<OplogPayload> = {}): OplogPayload {
  return { events: [], count: 0, limit: 100, error: '', ...overrides };
}

describe('oplogReading', () => {
  it('accepts canonical microseconds and rejects display timestamps on the wire', () => {
    const canonical = oplogPayload({
      events: [{ id: 1, ts: 1_754_006_400_000_000, op: 'created', fact_id: 'fact-project-7' }],
      count: 1,
    });
    expect(OplogPayloadSchema.safeParse(canonical).success).toBe(true);
    expect(
      OplogPayloadSchema.safeParse({
        ...canonical,
        events: [{ ...canonical.events[0], ts: '2026-08-01T00:00:00Z' }],
      }).success,
    ).toBe(false);
    expect(
      OplogPayloadSchema.safeParse({
        ...canonical,
        events: [{ ...canonical.events[0], ts: 1.5 }],
      }).success,
    ).toBe(false);
  });

  it('tallies canonical operations without inventing unavailable details', () => {
    const reading = oplogReading(
      oplogPayload({
        events: [
          { id: 1, ts: 1_754_006_400_000_000, op: 'created', fact_id: 'fact-project-7' },
          { id: 2, ts: 1_754_092_800_000_000, op: 'created', fact_id: 'fact-project-8' },
          { id: 3, ts: 1_754_179_200_000_000, op: 'payload_access_changed', fact_id: null },
        ],
        count: 3,
      }),
    );
    expect(reading.operations).toEqual([
      { op: 'created', count: 2 },
      { op: 'payload_access_changed', count: 1 },
    ]);
  });

  it('separates an unreadable store from a store nothing has written to', () => {
    expect(oplogReading(oplogPayload({ error: 'database is locked' })).storeError).toBe(
      'database is locked',
    );
    expect(oplogReading(oplogPayload()).storeError).toBeNull();
  });
});
