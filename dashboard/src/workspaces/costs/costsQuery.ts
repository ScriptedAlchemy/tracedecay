import { useCallback } from 'react';
import { useSearchParams } from 'react-router';

/**
 * The Costs query, held in the address so a link reopens the exact reading.
 *
 * Two view-local dimensions: the time range the daemon is asked to attribute
 * over, and the provider the reader has scoped to. Both replace rather than
 * push, because moving a range control four times is not four places to go
 * back to. Global project scope stays on `scope`/`scopeLabel`, owned by the
 * shell; this module never touches those keys.
 */

export type CostsRange = 'today' | '7d' | '30d' | 'all';

export const COSTS_RANGES: readonly CostsRange[] = ['today', '7d', '30d', 'all'];

export const COSTS_RANGE_PARAM = 'range';
export const COSTS_PROVIDER_PARAM = 'provider';

export function costsRangeLabel(range: CostsRange): string {
  switch (range) {
    case 'today':
      return 'Today';
    case '7d':
      return '7 days';
    case '30d':
      return '30 days';
    case 'all':
      return 'All time';
    default: {
      const unhandled: never = range;
      return unhandled;
    }
  }
}

/** What the daemon is asked for, printed beside the control so the reader
 * knows which window every figure on the surface belongs to. */
export function costsRangeNote(range: CostsRange): string {
  switch (range) {
    case 'today':
      return 'usage observed since 00:00 UTC today';
    case '7d':
      return 'usage observed in the last 7 × 24 hours';
    case '30d':
      return 'usage observed in the last 30 × 24 hours';
    case 'all':
      return 'every retained usage observation, dated or not';
    default: {
      const unhandled: never = range;
      return unhandled;
    }
  }
}

export function asCostsRange(value: string | null): CostsRange {
  switch (value) {
    case 'today':
    case '7d':
    case '30d':
      return value;
    // An absent or unreadable parameter reads the whole ledger: it is the
    // only window whose totals reconcile with the canonical all-time read.
    default:
      return 'all';
  }
}

export interface CostsQuery {
  range: CostsRange;
  /** The scoped provider, or `null` for every provider. */
  provider: string | null;
  setRange: (range: CostsRange) => void;
  setProvider: (provider: string | null) => void;
}

export function useCostsQuery(): CostsQuery {
  const [params, setParams] = useSearchParams();
  const range = asCostsRange(params.get(COSTS_RANGE_PARAM));
  const rawProvider = params.get(COSTS_PROVIDER_PARAM);
  const provider = rawProvider === null || rawProvider === '' ? null : rawProvider;

  const setRange = useCallback(
    (next: CostsRange) => {
      const updated = new URLSearchParams(params);
      if (next === 'all') updated.delete(COSTS_RANGE_PARAM);
      else updated.set(COSTS_RANGE_PARAM, next);
      setParams(updated, { replace: true });
    },
    [params, setParams],
  );

  const setProvider = useCallback(
    (next: string | null) => {
      const updated = new URLSearchParams(params);
      if (next === null) updated.delete(COSTS_PROVIDER_PARAM);
      else updated.set(COSTS_PROVIDER_PARAM, next);
      setParams(updated, { replace: true });
    },
    [params, setParams],
  );

  return { range, provider, setRange, setProvider };
}
