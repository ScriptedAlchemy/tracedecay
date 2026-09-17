import { describe, expect, it } from 'vitest';

import type { MemoryFactRowV1 } from '../../contracts/generated.ts';
import { asFactSort, factSortLabel, FACT_SORTS, ledgerDay, shortFactId, sortFacts } from './ledger.ts';

function fact(over: Partial<MemoryFactRowV1> & { fact_id: string }): MemoryFactRowV1 {
  return {
    payload_access: 'eligible',
    trust_score: 0.5,
    retrieval_count: 0,
    access_count: 0,
    helpful_count: 0,
    unhelpful_count: 0,
    created_at: 1_000,
    updated_at: 1_000,
    last_recalled_at: null,
    projected_as_of: 1_000,
    content: 'fixture',
    category: 'general',
    tags: [],
    entities: [],
    metadata: {},
    source_label: null,
    linked_entities: null,
    ...over,
  };
}

describe('sortFacts', () => {
  const rows = [
    fact({ fact_id: 'b', trust_score: 0.9, created_at: 300, last_recalled_at: 5, retrieval_count: 2, content: 'beta' }),
    fact({ fact_id: 'a', trust_score: null, created_at: 100, last_recalled_at: null, retrieval_count: null, content: 'alpha' }),
    fact({ fact_id: 'c', trust_score: 0.9, created_at: 200, last_recalled_at: 9, retrieval_count: 7, content: 'gamma' }),
  ];

  it('orders by trust with unreported trust last and ties broken by id', () => {
    expect(sortFacts(rows, 'trust').map((row) => row.fact_id)).toEqual(['b', 'c', 'a']);
  });

  it('orders by last recalled with never-recalled rows last', () => {
    expect(sortFacts(rows, 'recalled').map((row) => row.fact_id)).toEqual(['c', 'b', 'a']);
  });

  it('orders by created, newest first', () => {
    expect(sortFacts(rows, 'created').map((row) => row.fact_id)).toEqual(['b', 'c', 'a']);
  });

  it('orders by recall count with unreported counts last', () => {
    expect(sortFacts(rows, 'recalls').map((row) => row.fact_id)).toEqual(['c', 'b', 'a']);
  });

  it('orders by content alphabetically', () => {
    expect(sortFacts(rows, 'content').map((row) => row.fact_id)).toEqual(['a', 'b', 'c']);
  });

  it('does not mutate the loaded slice', () => {
    const before = rows.map((row) => row.fact_id);
    sortFacts(rows, 'content');
    expect(rows.map((row) => row.fact_id)).toEqual(before);
  });
});

describe('sort parameter and labels', () => {
  it('reads every sort from the address and defaults to the server ranking', () => {
    for (const sort of FACT_SORTS) expect(asFactSort(sort)).toBe(sort);
    expect(asFactSort(null)).toBe('trust');
    expect(asFactSort('nonsense')).toBe('trust');
  });

  it('names every sort', () => {
    for (const sort of FACT_SORTS) expect(factSortLabel(sort).length).toBeGreaterThan(0);
  });
});

describe('column readings', () => {
  it('prints a canonical microsecond stamp as its UTC day and a null as the named sentinel', () => {
    expect(ledgerDay(1_784_000_000_000_000, 'never')).toBe('2026-07-14');
    expect(ledgerDay(null, 'never')).toBe('never');
    expect(ledgerDay(Number.NaN, '—')).toBe('—');
  });

  it('shortens a canonical fact id to its distinguishing tail', () => {
    const id = `fact.${'a'.repeat(64)}.${'0'.repeat(60)}beef`;
    expect(shortFactId(id)).toBe('…000000beef');
    expect(shortFactId('fact-project-7')).toBe('fact-project-7');
  });
});
