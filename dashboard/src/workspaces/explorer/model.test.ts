import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  LANES,
  codeHits,
  facetCounts,
  knowledgeHits,
  relativeTime,
  sessionHits,
} from './model.ts';

afterEach(() => {
  vi.useRealTimers();
});

function canonicalFactId(seed: number): string {
  return `fact.${'a'.repeat(64)}.${seed.toString(16).padStart(64, '0')}`;
}

describe('codeHits', () => {
  it('preserves daemon order and labels degree as a measured field, not relevance', () => {
    const hits = codeHits(
      [
        {
          id: 'n2',
          name: 'search_payload',
          kind: 'function',
          file_path: 'src/dashboard/graph_service.rs',
          start_line: 274,
          degree: 4,
        },
        { id: 'n1', qualified_name: 'fallback::row', degree: 9 },
      ],
      ['search'],
    );

    expect(hits.map((hit) => [hit.key, hit.rank])).toEqual([
      ['code:n2', 1],
      ['code:n1', 2],
    ]);
    expect(hits[0]?.context).toBe('src/dashboard/graph_service.rs:274');
    expect(hits[0]?.contextFields).toEqual(['file_path', 'start_line']);
    expect(hits[0]?.matchedIn).toEqual(['name']);
    expect(hits[0]?.orderLabel).toBe('graph endpoint rows');
    expect(hits[0]?.signal).toEqual({
      field: 'degree',
      value: 4,
      max: 9,
      display: '4 edges',
      basis: 'maximum among loaded graph rows',
    });
  });

  it('uses a returned identifier instead of inventing a row title', () => {
    const [hit] = codeHits([{ id: 'node-without-name' }], []);

    expect(hit).toMatchObject({
      title: 'node-without-name',
      titleField: 'id',
    });
  });
});

describe('sessionHits', () => {
  it('normalizes both message and summary-node rows from the real LCM response', () => {
    const hits = sessionHits(
      [
        {
          message_id: 'm1',
          session_id: 's1',
          provider: 'cursor',
          role: 'assistant',
          snippet: 'Found the graph route',
          timestamp: 100,
        },
        {
          node_id: 'sum1',
          session_id: 's2',
          summary: 'Graph endpoint investigation',
          latest_at: 200,
        },
      ],
      ['graph'],
    );

    expect(hits).toHaveLength(2);
    expect(hits[0]).toMatchObject({
      key: 'sessions:m1',
      rank: 1,
      orderLabel: 'message matches',
      facet: 'assistant',
      context: 'cursor · s1',
      contextFields: ['provider', 'session_id'],
      titleField: 'snippet',
      stampField: 'timestamp',
    });
    expect(hits[1]).toMatchObject({
      key: 'sessions:sum1',
      rank: 1,
      orderLabel: 'summary-node matches',
      facet: 'summary',
      context: 's2',
      contextFields: ['session_id'],
      title: 'Graph endpoint investigation',
      titleField: 'summary',
      stampField: 'latest_at',
    });
  });
});

describe('knowledgeHits', () => {
  it('retains fact trust and category without inventing missing values', () => {
    const factId = canonicalFactId(42);
    const [hit] = knowledgeHits(
      [
        {
          fact_id: factId,
          content: 'Use the graph endpoint',
          category: 'decision',
          tags: ['dashboard', 'api'],
          trust_score: 0.75,
          last_recalled_at: 1_754_006_400_000_000,
        },
      ],
      ['graph'],
    );

    expect(hit).toMatchObject({
      key: `knowledge:${factId}`,
      facet: 'decision',
      context: 'dashboard · api',
      contextFields: ['tags'],
      matchedIn: ['content'],
      stamp: 1_754_006_400,
      stampField: 'last_recalled_at',
      orderLabel: 'bounded fact endpoint rows',
      signal: {
        field: 'trust_score',
        value: 0.75,
        max: 1,
        display: 'trust 0.75',
        basis: 'fixed trust scale from 0 to 1',
      },
    });
  });

  it('keeps a nullable trust score absent instead of manufacturing zero trust', () => {
    const [hit] = knowledgeHits(
      [{ fact_id: canonicalFactId(43), content: 'Trust has not been projected', trust_score: null }],
      [],
    );

    expect(hit?.signal).toBeUndefined();
  });
});

describe('LANES', () => {
  it('describes the bounded canonical fields without claiming evidence or all providers', () => {
    expect(LANES.map(({ id, searches }) => [id, searches])).toEqual([
      ['code', 'name, qualified_name, signature, and file_path'],
      ['sessions', 'message content and summary text in the active LCM store'],
      ['knowledge', 'content and tags in a bounded fact overview'],
      [
        'semantic',
        'nothing yet from this surface — the coordinator reports the semantic provider’s typed state per run',
      ],
    ]);
  });
});

describe('facetCounts', () => {
  it('counts loaded rows only and sorts ties by label', () => {
    const hits = knowledgeHits(
      [
        { fact_id: canonicalFactId(1), content: 'one', category: 'project' },
        { fact_id: canonicalFactId(2), content: 'two', category: 'decision' },
        { fact_id: canonicalFactId(3), content: 'three', category: 'project' },
        { fact_id: canonicalFactId(4), content: 'four' },
      ],
      [],
    );
    expect(facetCounts(hits)).toEqual([
      { id: 'project', label: 'project', count: 2 },
      { id: 'decision', label: 'decision', count: 1 },
    ]);
  });
});

// The source-state cases that lived here moved to laneModel.test.ts when
// `plannerLaneState` became `laneFromSourceProgress`; both invariants they
// protected are asserted there against the same two shapes.

describe('relativeTime', () => {
  it('uses unix seconds and handles future observations explicitly', () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date('2026-07-25T18:00:00Z'));
    const now = Date.now() / 1000;
    expect(relativeTime(now + 10)).toBe('now');
    expect(relativeTime(now - 120)).toBe('2m');
    expect(relativeTime(undefined)).toBeUndefined();
  });
});
