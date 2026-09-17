/**
 * The Facts camera's position, kept in the address.
 *
 * Camera (`view`), applied query (`q`), sort (`sort`) and selected fact
 * (`fact`) are all URL parameters so a reload or a shared link reopens the
 * exact reading. All writes replace rather than push: panning across facts is
 * not a trail of places to go back to. Inspection — the fact under the pointer
 * or focus — is deliberately NOT in the address; it is transient by
 * definition and would otherwise rewrite the URL on every hover.
 *
 * Fact identity is owned by a project (`FactId::validate_owner`), so a
 * selected fact does not survive a change of scope: the hook clears it when
 * the scope key moves, rather than letting project A's identity be asked of
 * project B's store.
 */
import { useCallback, useEffect, useRef } from 'react';
import { useSearchParams } from 'react-router';

import { scopeKey, useScope } from '../../data/scope/store.ts';
import { asFactSort, type FactSort } from './ledger.ts';

export const FACT_PARAM = 'fact';
export const QUERY_PARAM = 'q';
export const SORT_PARAM = 'sort';

export function useFactsAddress(): {
  selectedFactId: string | null;
  selectFact: (factId: string | null) => void;
  query: string;
  applyQuery: (query: string) => void;
  sort: FactSort;
  setSort: (sort: FactSort) => void;
} {
  const [params, setParams] = useSearchParams();
  const scope = useScope((state) => state.scope);
  const currentScopeKey = scopeKey(scope);

  const write = useCallback(
    (mutate: (next: URLSearchParams) => void) => {
      const next = new URLSearchParams(params);
      mutate(next);
      setParams(next, { replace: true });
    },
    [params, setParams],
  );

  const selectedFactId = params.get(FACT_PARAM);
  const selectFact = useCallback(
    (factId: string | null) =>
      write((next) => {
        if (factId === null) next.delete(FACT_PARAM);
        else next.set(FACT_PARAM, factId);
      }),
    [write],
  );

  const query = params.get(QUERY_PARAM) ?? '';
  const applyQuery = useCallback(
    (value: string) =>
      write((next) => {
        const trimmed = value.trim();
        if (trimmed === '') next.delete(QUERY_PARAM);
        else next.set(QUERY_PARAM, trimmed);
      }),
    [write],
  );

  const sort = asFactSort(params.get(SORT_PARAM));
  const setSort = useCallback(
    (value: FactSort) =>
      write((next) => {
        if (value === 'trust') next.delete(SORT_PARAM);
        else next.set(SORT_PARAM, value);
      }),
    [write],
  );

  // A selected identity belongs to the scope it was selected in. The first
  // pass adopts the mounting scope (a deep link into a scoped store must keep
  // its fact); every later change of scope drops the selection.
  const seenScopeKey = useRef<string | null>(null);
  useEffect(() => {
    if (seenScopeKey.current === null) {
      // The store, not the render closure: the shell's URL→scope sync runs in
      // the same commit and may already have applied a deep link's scope that
      // this render has not seen yet. Adopting the closure's value would make
      // that first reconciliation look like a scope change and drop the fact
      // the link carried.
      seenScopeKey.current = scopeKey(useScope.getState().scope);
      return;
    }
    if (seenScopeKey.current === currentScopeKey) return;
    seenScopeKey.current = currentScopeKey;
    if (selectedFactId !== null) selectFact(null);
  }, [currentScopeKey, selectedFactId, selectFact]);

  return { selectedFactId, selectFact, query, applyQuery, sort, setSort };
}
