/**
 * How a two-hop neighbourhood is fetched — the trace surface's only knowledge
 * of it, and provisional on purpose.
 *
 * DEPTH. `GET /api/plugins/graph/node/{id}/neighbors` serves ONE hop. The field
 * draws two, so hop 2 is assembled on the client: hop 1 for the focus, then hop
 * 1 for as many of its drawn hop-1 neighbours as `TRACE_BUDGET.expand` allows,
 * deduped in `buildTraceModel` and counted in its `coverage`. Whatever the
 * bound leaves out is printed rather than dropped — see `coverageCaption`.
 *
 * WHY THIS IS ITS OWN MODULE. The backend does not yet expose a bounded
 * two-hop neighbourhood operation, so that fan-out is a stand-in for a query
 * that does not exist yet. A stand-in left inline spreads: a `limit=` beside
 * one component, a second wave of reads inside another, a list of neighbour ids
 * threaded through props — and the day the generated operation lands, adopting
 * it becomes a rewrite of the surface instead of a rewrite of one file. So the
 * route, the limit, the fan-out and its bound live here and nowhere else, and
 * what leaves is `TraceNeighborhood`: payloads and states, never queries.
 * Swapping this body for one generated call must be invisible above it.
 *
 * The bound is exactly what it says and no cleverer: one read for the focus,
 * then at most `TRACE_BUDGET.expand` reads for its neighbours in a single wave,
 * with no third hop, no prefetch, and no cache of its own beyond the query
 * client's.
 */
import { useMemo } from 'react';
import { useQueries } from '@tanstack/react-query';

import { fetchLegacy, type LegacyResult } from '../../data/query/legacy.ts';
import { useLegacy } from '../../data/query/useLegacy.ts';
import { scopeKey, scopedUrl, useScope } from '../../data/scope/store.ts';
import { TRACE_BUDGET } from '../../viz/trace/model.ts';
import {
  GraphNeighborsPayloadV1Schema,
  type GraphNeighborsPayloadV1,
} from '../../contracts/generated.ts';

const BASE = '/api/plugins/graph';
/** The endpoint's own hard cap (`coerce_limit(params.limit, 50, 200)`). */
const NEIGHBOR_LIMIT = 200;

function neighborsUrl(id: string): string {
  return `${BASE}/node/${encodeURIComponent(id)}/neighbors?limit=${NEIGHBOR_LIMIT}`;
}

/**
 * The hop-1 neighbours whose own neighbourhoods are read to assemble hop 2.
 *
 * Ordered by first appearance, which is the endpoint's own
 * `ORDER BY n.qualified_name` — stable across reloads, so the same
 * neighbourhood expands the same way twice. The focus is excluded because its
 * own payload is already in hand, and the cap is `TRACE_BUDGET.expand` because
 * this is one read per id.
 */
export function expansionTargets(
  payload: GraphNeighborsPayloadV1,
  focusId: string,
): readonly string[] {
  const seen = new Set<string>();
  for (const row of [...(payload.callers ?? []), ...(payload.callees ?? [])]) {
    if (row.id !== focusId) seen.add(row.id);
  }
  return [...seen].slice(0, TRACE_BUDGET.expand);
}

/**
 * What the surface is handed: the focus's own hop-1 read as a state it can
 * render, plus whichever expansions have answered so far.
 *
 * Deliberately not a query object. A caller that could reach `refetch`, a
 * query key or a URL from here would be holding a second opinion about how
 * this surface gets its data, which is the thing this module exists to prevent.
 */
export interface TraceNeighborhood {
  /** The focus's own hop-1 read has not answered yet. */
  readonly pending: boolean;
  /** Its outcome, or `undefined` before the first response. */
  readonly result: LegacyResult<GraphNeighborsPayloadV1> | undefined;
  /**
   * Hop-1 payloads of the expanded neighbours, keyed by neighbour id. A
   * neighbour that failed is simply absent, which the model counts.
   */
  readonly expanded: ReadonlyMap<string, GraphNeighborsPayloadV1>;
  /** At least one expansion is still in flight. */
  readonly expanding: boolean;
}

/**
 * The focus's neighbourhood plus, for as many of its drawn neighbours as the
 * budget allows, theirs. Expansion is a second wave of independent reads, so a
 * neighbour that fails leaves a hole that gets COUNTED rather than one that
 * takes the surface down.
 */
export function useTraceNeighborhood(focusId: string): TraceNeighborhood {
  const scope = useScope((s) => s.scope);
  const root = useLegacy(
    ['graph', 'neighbors', focusId],
    neighborsUrl(focusId),
    GraphNeighborsPayloadV1Schema,
  );

  const hop1 = useMemo<readonly string[]>(
    () => (root.data?.outcome === 'ok' ? expansionTargets(root.data.data, focusId) : []),
    [root.data, focusId],
  );

  const expansions = useQueries({
    queries: hop1.map((id) => ({
      queryKey: ['graph', 'neighbors', id, scopeKey(scope)],
      queryFn: () =>
        fetchLegacy(scopedUrl(scope, neighborsUrl(id)), GraphNeighborsPayloadV1Schema),
      staleTime: 60_000,
    })),
  });

  // `useQueries` returns a fresh array every render, so memoising on it would
  // rebuild the model — and therefore tear down and re-seed the simulation —
  // sixty times a second. The identity that actually matters is which ids have
  // settled, which is exactly what this signature carries.
  const signature = hop1.map((id, i) => `${id}:${expansions[i]?.status ?? 'idle'}`).join('|');
  const expanded = useMemo(() => {
    const out = new Map<string, GraphNeighborsPayloadV1>();
    hop1.forEach((id, i) => {
      const result = expansions[i]?.data as LegacyResult<GraphNeighborsPayloadV1> | undefined;
      if (result?.outcome === 'ok') out.set(id, result.data);
    });
    return out;
  }, [signature]);

  return {
    pending: root.isPending,
    result: root.data,
    expanded,
    expanding: expansions.some((q) => q.isPending),
  };
}
