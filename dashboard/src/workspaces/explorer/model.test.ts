import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  codeHits,
  facetCounts,
  knowledgeHits,
  relativeTime,
  sessionHits,
} from './model.ts';

afterEach(() => {
  vi.useRealTimers();
});

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
    expect(hits[0]?.matchedIn).toEqual(['name']);
    expect(hits[0]?.signal).toEqual({
      field: 'degree',
      value: 4,
      max: 9,
      display: '4 edges',
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
      facet: 'assistant',
      context: 'cursor · s1',
      titleField: 'snippet',
    });
    expect(hits[1]).toMatchObject({
      key: 'sessions:sum1',
      facet: 'summary',
      context: 's2',
      title: 'Graph endpoint investigation',
      titleField: 'summary',
    });
  });
});

describe('knowledgeHits', () => {
  it('retains fact trust and category without inventing missing values', () => {
    const [hit] = knowledgeHits(
      [
        {
          fact_id: 42,
          content: 'Use the graph endpoint',
          category: 'decision',
          tags: ['dashboard', 'api'],
          trust_score: 0.75,
        },
      ],
      ['graph'],
    );

    expect(hit).toMatchObject({
      key: 'knowledge:42',
      facet: 'decision',
      context: 'dashboard · api',
      matchedIn: ['content'],
      signal: {
        field: 'trust_score',
        value: 0.75,
        max: 1,
        display: 'trust 0.75',
      },
    });
  });
});

describe('facetCounts', () => {
  it('counts loaded rows only and sorts ties by label', () => {
    const hits = knowledgeHits(
      [
        { fact_id: 1, content: 'one', category: 'project' },
        { fact_id: 2, content: 'two', category: 'decision' },
        { fact_id: 3, content: 'three', category: 'project' },
        { fact_id: 4, content: 'four' },
      ],
      [],
    );
    expect(facetCounts(hits)).toEqual([
      { id: 'project', label: 'project', count: 2 },
      { id: 'decision', label: 'decision', count: 1 },
    ]);
  });
});

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
