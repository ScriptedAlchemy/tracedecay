/**
 * The fact ledger's pure readings: how the loaded slice is ordered and what
 * each column prints.
 *
 * Ordering is over the LOADED slice only. The overview route serves at most a
 * hundred rows ranked by trust and exposes no cursor, so a sort here can
 * reorder what arrived but can never reach a row it did not — the ledger says
 * so above the rows, and nothing in this module pretends otherwise.
 */
import { assertNever, type MemoryFactRowV1 } from '../../contracts/generated.ts';

export type FactSort = 'trust' | 'recalled' | 'created' | 'recalls' | 'content';

export const FACT_SORTS: readonly FactSort[] = [
  'trust',
  'recalled',
  'created',
  'recalls',
  'content',
];

export function factSortLabel(sort: FactSort): string {
  switch (sort) {
    case 'trust':
      return 'trust, highest first';
    case 'recalled':
      return 'last recalled, newest first';
    case 'created':
      return 'created, newest first';
    case 'recalls':
      return 'recall count, highest first';
    case 'content':
      return 'content, A to Z';
    default:
      return assertNever(sort);
  }
}

/** Parse a URL parameter into a sort, defaulting to the order the server
 * itself ranked the slice in. */
export function asFactSort(value: string | null): FactSort {
  switch (value) {
    case 'recalled':
    case 'created':
    case 'recalls':
    case 'content':
      return value;
    default:
      return 'trust';
  }
}

function descending(a: number | null | undefined, b: number | null | undefined): number {
  // Unreported values sort last under every ordering: an absent measurement is
  // not a small one.
  const left = typeof a === 'number' && Number.isFinite(a) ? a : null;
  const right = typeof b === 'number' && Number.isFinite(b) ? b : null;
  if (left === null && right === null) return 0;
  if (left === null) return 1;
  if (right === null) return -1;
  return right - left;
}

/** A stable sort of the loaded rows; ties fall back to fact id so the same
 * slice always orders the same way. */
export function sortFacts(
  facts: readonly MemoryFactRowV1[],
  sort: FactSort,
): MemoryFactRowV1[] {
  const compare = (a: MemoryFactRowV1, b: MemoryFactRowV1): number => {
    switch (sort) {
      case 'trust':
        return descending(a.trust_score, b.trust_score);
      case 'recalled':
        return descending(a.last_recalled_at, b.last_recalled_at);
      case 'created':
        return descending(a.created_at, b.created_at);
      case 'recalls':
        return descending(a.retrieval_count, b.retrieval_count);
      case 'content':
        return (a.content ?? '').localeCompare(b.content ?? '');
      default:
        return assertNever(sort);
    }
  };
  return [...facts].sort((a, b) => compare(a, b) || a.fact_id.localeCompare(b.fact_id));
}

/** A canonical microsecond stamp as the calendar day a dense row can carry;
 * the full instant is on the inspector. `null` is printed as the sentinel the
 * caller names, never as a date. */
export function ledgerDay(micros: number | null | undefined, nullAs: string): string {
  if (micros == null || !Number.isFinite(micros)) return nullAs;
  const date = new Date(Math.floor(micros / 1000));
  if (Number.isNaN(date.getTime())) return nullAs;
  return date.toISOString().slice(0, 10);
}

/** A canonical fact id as the ledger prints it. Ids are `fact.<owner>.<hash>`
 * with two 64-hex segments; the tail is the part that tells two apart. */
export function shortFactId(factId: string): string {
  const tail = factId.split('.').at(-1) ?? factId;
  return tail.length > 24 ? `…${tail.slice(-10)}` : tail;
}
