/**
 * Explorer's orchestration: every route it calls, every piece of query state it
 * holds, and the typed lane read models it hands to the views.
 *
 * Kept apart from the JSX so the surface has exactly one place that knows a
 * search is opened, folded as its sources publish, and closed, and so the views
 * below it render a lane condition rather than deciding one.
 */
import { useQuery } from '@tanstack/react-query';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  ExplorerReadContextV1Schema,
  ExplorerSessionSizeV1Schema,
  GraphOverviewPayloadV1Schema,
  LcmOverviewPayloadV1Schema,
  MemoryOverviewPayloadV1Schema,
  type ExplorerReadContextV1,
  type ExplorerSessionSizeV1,
} from '../../contracts/generated.ts';
import { fetchEnvelope, type EnvelopeResult } from '../../data/query/envelope.ts';
import { useEnvelope } from '../../data/query/useEnvelope.ts';
import {
  scopeKey,
  scopeWritable,
  scopedUrl,
  useScope,
  type DashboardScope,
  type ScopeWritability,
} from '../../data/scope/store.ts';
import { queryTerms } from '../../ui/search/terms.ts';
import { absenceVerdict, type AbsenceVerdict } from './absence.ts';
import {
  browseLane,
  laneAnswered,
  laneFromScope,
  laneHits,
  lanePending,
  SEARCH_SOURCE_COUNT,
  searchLane,
  semanticLane,
  startedSearch,
  withStreamEvent,
  type ExplorerLaneReadModel,
  type ExplorerSearch,
  type ExplorerSearchResult,
} from './laneModel.ts';
import { streamExplorerSearch } from './searchStream.ts';
import { LANES, type Hit, type LaneId, type SourceLaneId } from './model.ts';

/* ------------------------------------------------------------------ routes */

const QUERIES_ROUTE = '/api/explorer/queries';
const QUERY_LIMIT = 50;

function readSessionSize(
  scope: DashboardScope,
  sessionId: string,
): Promise<EnvelopeResult<ExplorerSessionSizeV1>> {
  return fetchEnvelope(
    scopedUrl(scope, `/api/explorer/sessions/${encodeURIComponent(sessionId)}/size`),
    ExplorerSessionSizeV1Schema,
  );
}

function readSessionContext(
  scope: DashboardScope,
  sessionId: string,
): Promise<EnvelopeResult<ExplorerReadContextV1>> {
  return fetchEnvelope(
    scopedUrl(
      scope,
      `/api/explorer/sessions/${encodeURIComponent(sessionId)}/read-context?limit=25&offset=0&order=asc`,
    ),
    ExplorerReadContextV1Schema,
  );
}

/* -------------------------------------------------------------- controller */

interface ExplorerFacet {
  readonly lane: SourceLaneId;
  readonly value: string;
}

interface ScopeOwned<T> {
  readonly scope: string;
  readonly value: T;
}

/** How far the search has got, counted in sources rather than in a
 * percentage: sources have incommensurable units, so "2 of 3 concluded" is the
 * only progress figure with a real denominator. */
export interface SearchProgress {
  readonly concluded: number;
  readonly total: number;
}

export interface ExplorerController {
  readonly query: string;
  readonly submitted: string;
  readonly searching: boolean;
  readonly terms: readonly string[];
  /** Whether the current scope accepts a search, from the scope authority.
   * Anything but `writable` means no search was, or will be, opened. */
  readonly writability: ScopeWritability;
  /** One read model per lane, in `LANES` order, four, including semantic. */
  readonly lanes: readonly ExplorerLaneReadModel[];
  /** The lanes the current lane filter admits. */
  readonly visibleLanes: readonly ExplorerLaneReadModel[];
  /** Each lane's rows after the facet pivot. Empty for lanes without rows. */
  readonly laneRows: ReadonlyMap<LaneId, readonly Hit[]>;
  readonly anyPending: boolean;
  /** Lanes that neither answered nor are still working. */
  readonly unansweredLanes: readonly ExplorerLaneReadModel[];
  readonly answeredLaneCount: number;
  readonly absence: AbsenceVerdict;
  readonly searchResult: ExplorerSearchResult | undefined;
  /** The search for the submitted query, once its response has opened. */
  readonly search: ExplorerSearch | undefined;
  readonly searchProgress: SearchProgress | null;
  /** Present only while the submitted search is streaming. */
  readonly cancel: (() => void) | undefined;
  readonly laneFilter: LaneId | null;
  readonly facet: ExplorerFacet | null;
  /** The row a click or Enter picked. Persistent until cleared. */
  readonly selected: Hit | null;
  /** The row the pointer or keyboard focus is resting on. Transient; never a
   * selection, and never a reason to fetch. */
  readonly peeked: Hit | null;
  readonly setQuery: (value: string) => void;
  /** Submit the field's text, or a given query (a URL restoring one). */
  readonly submit: (value?: string) => void;
  readonly reset: () => void;
  readonly setLaneFilter: (lane: LaneId | null) => void;
  readonly setFacet: (facet: ExplorerFacet | null) => void;
  readonly select: (hit: Hit | null) => void;
  readonly peek: (hit: Hit | null) => void;
}

export function useExplorerController(): ExplorerController {
  const scope = useScope((s) => s.scope);
  const writability = useMemo(() => scopeWritable(scope), [scope]);
  const requestScope = `${scopeKey(scope)}:${writability.state}`;
  const [query, setQuery] = useState('');
  const [submitted, setSubmitted] = useState('');
  const [heldResult, setSearchResult] = useState<ScopeOwned<ExplorerSearchResult> | undefined>();
  // The one response currently allowed to write `searchResult`. Aborting it
  // closes the response, which stops the reads the server still had open.
  const openSearch = useRef<AbortController | null>(null);
  const [laneFilter, setLaneFilter] = useState<LaneId | null>(null);
  const [facet, setFacet] = useState<ExplorerFacet | null>(null);
  const [heldSelection, setSelected] = useState<ScopeOwned<Hit> | null>(null);
  const [heldPeek, setPeeked] = useState<ScopeOwned<Hit> | null>(null);
  const searchResult = heldResult?.scope === requestScope ? heldResult.value : undefined;
  const selected = heldSelection?.scope === requestScope ? heldSelection.value : null;
  const peeked = heldPeek?.scope === requestScope ? heldPeek.value : null;
  const searching = submitted !== '';
  const terms = useMemo(() => queryTerms(submitted), [submitted]);

  const search =
    searchResult?.outcome === 'search' && searchResult.search.query === submitted
      ? searchResult.search
      : undefined;

  const graphBrowse = useEnvelope(
    ['explorer', 'graph-overview'],
    '/api/plugins/graph/overview',
    GraphOverviewPayloadV1Schema,
    { enabled: !searching },
  );
  const lcmBrowse = useEnvelope(
    ['explorer', 'lcm-overview'],
    '/api/plugins/hermes-lcm/overview',
    LcmOverviewPayloadV1Schema,
    { enabled: !searching },
  );
  const memory = useEnvelope(
    ['explorer', 'memory-overview'],
    '/api/plugins/holographic?limit=25',
    MemoryOverviewPayloadV1Schema,
    { enabled: !searching },
  );
  const graphBrowseData = graphBrowse.data;
  const graphBrowsePending = graphBrowse.isPending;
  const lcmBrowseData = lcmBrowse.data;
  const lcmBrowsePending = lcmBrowse.isPending;
  const memoryData = memory.data;
  const memoryPending = memory.isPending;

  const lanes = useMemo<readonly ExplorerLaneReadModel[]>(
    () =>
      LANES.map((spec): ExplorerLaneReadModel => {
        if (spec.id === 'semantic') return semanticLane();
        if (searching) {
          // A search is a POST, and the project gateway refuses writes for
          // every project but the active one. The gate is read here, before
          // any request, so a refused scope is rendered from the scope
          // authority's own reason and nothing is asked of a daemon that would
          // have to say no.
          const refused = laneFromScope(spec.id, writability);
          if (refused !== null) return refused;
          return searchLane(spec.id, searchResult, submitted, terms);
        }
        switch (spec.id) {
          case 'code':
            return browseLane(
              'code',
              graphBrowseData,
              graphBrowsePending,
              (data) => data.top_connected,
              terms,
            );
          case 'sessions':
            return browseLane(
              'sessions',
              lcmBrowseData,
              lcmBrowsePending,
              (data) => data.latest_summary_nodes,
              terms,
            );
          case 'knowledge':
            return browseLane(
              'knowledge',
              memoryData,
              memoryPending,
              (data) => data.holographic.facts,
              terms,
            );
          default: {
            const exhaustive: never = spec.id;
            return exhaustive;
          }
        }
      }),
    [
      graphBrowseData,
      graphBrowsePending,
      lcmBrowseData,
      lcmBrowsePending,
      memoryData,
      memoryPending,
      searchResult,
      searching,
      submitted,
      terms,
      writability,
    ],
  );

  const { visibleLanes, laneRows } = useMemo(() => {
    const visibleLanes = laneFilter ? lanes.filter((lane) => lane.lane === laneFilter) : lanes;
    const laneRows = new Map<LaneId, readonly Hit[]>(
      lanes.map((lane) => {
        const rows = laneHits(lane);
        return [
          lane.lane,
          facet && facet.lane === lane.lane ? rows.filter((hit) => hit.facet === facet.value) : rows,
        ];
      }),
    );
    return { visibleLanes, laneRows };
  }, [facet, laneFilter, lanes]);

  const closeSearch = useCallback(() => {
    openSearch.current?.abort();
    openSearch.current = null;
  }, []);
  // Leaving the surface closes the response, and with it the reads it held.
  useEffect(() => closeSearch, [closeSearch]);

  const dispatch = useCallback((nextQuery: string) => {
    // A newer search supersedes the open one: its response is closed, and any
    // frame already in flight is dropped by the guard below.
    closeSearch();
    setSearchResult(undefined);
    // Nothing is opened under a scope that will refuse it; the lanes render
    // the refusal instead.
    if (writability.state !== 'writable') return;
    const controller = new AbortController();
    openSearch.current = controller;
    const update = (change: (current: ExplorerSearch) => ExplorerSearch) => {
      if (controller.signal.aborted) return;
      setSearchResult((current) =>
        !controller.signal.aborted && current?.scope === requestScope && current.value.outcome === 'search'
          ? { scope: requestScope, value: { outcome: 'search', search: change(current.value.search) } }
          : current,
      );
    };
    setSearchResult({ scope: requestScope, value: { outcome: 'search', search: startedSearch(nextQuery) } });
    streamExplorerSearch(
      scopedUrl(scope, QUERIES_ROUTE),
      { query: nextQuery, limit: QUERY_LIMIT },
      controller.signal,
      (event) => update((current) => withStreamEvent(current, event)),
    ).then(
      (failure) => {
        if (controller.signal.aborted || failure === null) return;
        if (failure.outcome === 'interrupted') {
          update((current) => ({ ...current, status: 'interrupted' }));
        } else {
          setSearchResult({ scope: requestScope, value: failure });
        }
      },
      () => {
        if (!controller.signal.aborted) update((current) => ({ ...current, status: 'interrupted' }));
      },
    );
  }, [closeSearch, requestScope, scope, writability]);

  // The rows on screen must belong to the scope in the register. When the
  // scope changes under a submitted query, a project selected, a deep link
  // resolving from `unresolved` to `active`, the run is re-created against
  // the new scope rather than left showing the old project's answer.
  const lastRequestScope = useRef(requestScope);
  useEffect(() => {
    if (lastRequestScope.current === requestScope) return;
    lastRequestScope.current = requestScope;
    if (submitted === '') return;
    setSelected(null);
    setPeeked(null);
    dispatch(submitted);
  }, [requestScope, dispatch, submitted]);

  const reset = () => {
    closeSearch();
    setSearchResult(undefined);
    setQuery('');
    setSubmitted('');
    setFacet(null);
    setSelected(null);
    setPeeked(null);
  };

  const searchProgress: SearchProgress | null =
    search === undefined
      ? null
      : { concluded: search.sources.length, total: SEARCH_SOURCE_COUNT };

  return {
    query,
    submitted,
    searching,
    terms,
    writability,
    lanes,
    visibleLanes,
    laneRows,
    anyPending: lanes.some((lane) => lanePending(lane)),
    unansweredLanes: lanes.filter((lane) => !laneAnswered(lane) && !lanePending(lane)),
    answeredLaneCount: lanes.filter((lane) => laneAnswered(lane)).length,
    // A confirmed global absence is a claim about the whole index, so it is
    // re-derived from the sources' own unit accounting rather than reprinted
    // from the final outcome. See `absence.ts`.
    absence: absenceVerdict(search),
    searchResult,
    search,
    searchProgress,
    cancel:
      search?.status === 'streaming'
        ? () => {
            closeSearch();
            setSearchResult({
              scope: requestScope,
              value: { outcome: 'search', search: { ...search, status: 'cancelled' } },
            });
          }
        : undefined,
    laneFilter,
    facet,
    selected,
    peeked,
    setQuery,
    submit: (value) => {
      const nextQuery = (value ?? query).trim();
      if (nextQuery === '') return;
      if (value !== undefined) setQuery(value);
      setSubmitted(nextQuery);
      setFacet(null);
      setSelected(null);
      setPeeked(null);
      dispatch(nextQuery);
    },
    reset,
    setLaneFilter: (lane) => {
      setLaneFilter(lane);
      setFacet(null);
    },
    setFacet,
    select: (hit) => setSelected(hit === null ? null : { scope: requestScope, value: hit }),
    peek: (hit) => setPeeked(hit === null ? null : { scope: requestScope, value: hit }),
  };
}

/* ------------------------------------------------------- session inspector */

interface ExplorerSessionContext {
  readonly size: EnvelopeResult<ExplorerSessionSizeV1> | undefined;
  readonly readContext: EnvelopeResult<ExplorerReadContextV1> | undefined;
  readonly pending: boolean;
}

/** The two session reads the inspector shows for a transcript row. Disabled
 * while the row is only being peeked at: a hover is an inspection of what is
 * already on screen, not a reason to open two more reads. */
export function useExplorerSessionContext(
  sessionId: string | undefined,
  enabled = true,
): ExplorerSessionContext {
  const scope = useScope((s) => s.scope);
  const sessionIdForQuery = sessionId ?? '';
  const active = enabled && sessionIdForQuery !== '';
  const size = useQuery({
    queryKey: ['explorer', 'session-size', scopeKey(scope), sessionIdForQuery],
    queryFn: () => readSessionSize(scope, sessionIdForQuery),
    enabled: active,
  });
  const readContext = useQuery({
    queryKey: ['explorer', 'read-context', scopeKey(scope), sessionIdForQuery],
    queryFn: () => readSessionContext(scope, sessionIdForQuery),
    enabled: active,
  });
  return {
    size: size.data,
    readContext: readContext.data,
    pending: active && (size.isPending || readContext.isPending),
  };
}
