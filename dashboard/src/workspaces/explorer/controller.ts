/**
 * Explorer's orchestration: every route it calls, every piece of query state it
 * holds, and the typed lane read models it hands to the views.
 *
 * Kept apart from the JSX so the surface has exactly one place that knows a
 * coordinator run is created, polled, and cancelled, and so the views below it
 * render a lane condition rather than deciding one.
 */
import { useMutation, useQuery } from '@tanstack/react-query';
import { useEffect, useMemo, useRef, useState } from 'react';
import {
  ExplorerQueryRunV1Schema,
  ExplorerReadContextV1Schema,
  ExplorerSessionSizeV1Schema,
  GraphOverviewPayloadV1Schema,
  LcmOverviewPayloadV1Schema,
  MemoryOverviewPayloadV1Schema,
  type DashboardDomainStateV1,
  type ExplorerQueryRunV1,
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
  runIsTerminal,
  searchLane,
  semanticLane,
  type ExplorerLaneReadModel,
} from './laneModel.ts';
import { LANES, type Hit, type LaneId, type SourceLaneId } from './model.ts';

/* ------------------------------------------------------------------ routes */

const QUERIES_ROUTE = '/api/explorer/queries';

/**
 * Every Explorer route is rewritten for the current scope. A selected project
 * routes through the project gateway so the run is created against THAT
 * project's state; the all-projects default and the active project stay
 * unprefixed. Without this a query under a selected scope would silently be
 * answered by the active project, and the lanes would attribute one
 * project's rows to another's name in the scope register.
 */
function runRoute(scope: DashboardScope, runId?: string): string {
  const path = runId === undefined ? QUERIES_ROUTE : `${QUERIES_ROUTE}/${encodeURIComponent(runId)}`;
  return scopedUrl(scope, path);
}

function createPlannerQuery(
  scope: DashboardScope,
  query: string,
): Promise<EnvelopeResult<ExplorerQueryRunV1>> {
  return fetchEnvelope(runRoute(scope), ExplorerQueryRunV1Schema, {
    method: 'POST',
    headers: {
      accept: 'application/json',
      'content-type': 'application/json',
    },
    body: JSON.stringify({ query, limit: 50, offset: 0 }),
  });
}

function readPlannerQuery(
  scope: DashboardScope,
  runId: string,
): Promise<EnvelopeResult<ExplorerQueryRunV1>> {
  return fetchEnvelope(runRoute(scope, runId), ExplorerQueryRunV1Schema);
}

function cancelPlannerQuery(
  scope: DashboardScope,
  runId: string,
): Promise<EnvelopeResult<ExplorerQueryRunV1>> {
  return fetchEnvelope(runRoute(scope, runId), ExplorerQueryRunV1Schema, { method: 'DELETE' });
}

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

/* ------------------------------------------------------- run-status polling */

/**
 * Transport states a repeat read can clear on its own: the daemon was
 * unreachable, answered a bare non-2xx, or said it had nothing ready yet.
 *
 * Every other transport state is a standing condition, a refusal
 * (`unauthorized`, `denied`), a scope that will not serve the read (`locked`),
 * a body this client cannot read (`unsupported_schema`), and re-asking gets
 * the same answer, so the poll stops and the state is rendered.
 */
const RETRYABLE_TRANSPORT_STATES: ReadonlySet<DashboardDomainStateV1> = new Set<
  DashboardDomainStateV1
>(['offline', 'error', 'loading']);

/**
 * How long a retried status read waits, by how many consecutive transport
 * failures precede it: 1 s, 2 s, 5 s, 10 s, and then the ceiling below for as
 * long as the failure lasts.
 *
 * A ladder rather than an attempt budget. An admitted run does not stop
 * existing because the daemon blinked, and this poll is the only thing that
 * can ever resolve it, so a budget strands every run that outlives it, the
 * daemon comes back, the run completes, and the surface never finds out.
 * Backing off instead keeps the run reachable while making a dead daemon cost
 * two reads a minute, and the failing reads stay on screen the whole time
 * because `fetchEnvelope` reports them as data the lanes render.
 */
const TRANSPORT_BACKOFF_MS: readonly number[] = [1000, 2000, 5000, 10_000];

/** The slowest the poll ever ticks, held until the run terminates or the
 * surface goes away. */
const TRANSPORT_BACKOFF_CEILING_MS = 30_000;

function transportBackoffMs(consecutiveFailures: number): number {
  return TRANSPORT_BACKOFF_MS[consecutiveFailures - 1] ?? TRANSPORT_BACKOFF_CEILING_MS;
}

/* -------------------------------------------------------------- controller */

export interface ExplorerFacet {
  readonly lane: SourceLaneId;
  readonly value: string;
}

/** How far the coordinator has got, counted in sources rather than in a
 * percentage: sources have incommensurable units, so "2 of 3 concluded" is the
 * only progress figure with a real denominator. */
export interface RunProgress {
  readonly concluded: number;
  readonly total: number;
}

export interface ExplorerController {
  readonly query: string;
  readonly submitted: string;
  readonly searching: boolean;
  readonly terms: readonly string[];
  /** Whether the current scope accepts a query run, from the scope authority.
   * Anything but `writable` means no run was, or will be, dispatched. */
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
  readonly runResult: EnvelopeResult<ExplorerQueryRunV1> | undefined;
  /** The coordinator run for the submitted query, when one has answered. */
  readonly run: ExplorerQueryRunV1 | undefined;
  readonly runProgress: RunProgress | null;
  readonly cancelling: boolean;
  /** Present only while a cancellable run is in flight. */
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
  const [query, setQuery] = useState('');
  const [submitted, setSubmitted] = useState('');
  const [activeRunId, setActiveRunId] = useState<string | null>(null);
  const [laneFilter, setLaneFilter] = useState<LaneId | null>(null);
  const [facet, setFacet] = useState<ExplorerFacet | null>(null);
  const [selected, setSelected] = useState<Hit | null>(null);
  const [peeked, setPeeked] = useState<Hit | null>(null);
  const searching = submitted !== '';
  const terms = useMemo(() => queryTerms(submitted), [submitted]);

  const planner = useMutation({
    mutationFn: (nextQuery: string) => createPlannerQuery(scope, nextQuery),
    onSuccess: (result) => {
      if (result.outcome === 'envelope') {
        setActiveRunId(result.envelope.payload.run_id);
      }
    },
  });
  const activeRunIdForQuery = activeRunId ?? '';
  // `fetchEnvelope` reports a transport failure as data rather than throwing,
  // so react-query's own failure count never moves and cannot pace the retry
  // below. This is that count, how far down the backoff ladder the poll has
  // walked, reset by the first read that lands an envelope.
  const transportFailures = useRef(0);
  const runStatus = useQuery({
    queryKey: ['explorer', 'query-run', scopeKey(scope), activeRunIdForQuery],
    queryFn: async () => {
      const result = await readPlannerQuery(scope, activeRunIdForQuery);
      transportFailures.current = result.outcome === 'transport' ? transportFailures.current + 1 : 0;
      return result;
    },
    enabled: activeRunIdForQuery !== '',
    refetchInterval: (queryState) => {
      const result = queryState.state.data;
      // No data yet: the first read has not landed, keep the fast tick.
      if (result === undefined) return 250;
      if (result.outcome === 'transport') {
        // This poll is the only thing that resolves an admitted run, run
        // completion publishes no targeted invalidation, so a transport
        // failure a repeat read could clear must not end it, at any count: a
        // run whose daemon blinks more times than some budget allows is still
        // a live run, and abandoning it is the stuck surface this poll exists
        // to prevent. What is bounded is the rate, not the attempts. A
        // standing refusal still stops immediately, because re-asking it only
        // ever gets the same answer.
        if (!RETRYABLE_TRANSPORT_STATES.has(result.state)) return false;
        return transportBackoffMs(transportFailures.current);
      }
      if (runIsTerminal(result.envelope.payload.state)) return false;
      // A long-pending run escalates off the fast tick rather than holding
      // 250 ms for its whole life: 250 ms → 1 s → 2 s.
      const reads = queryState.state.dataUpdateCount;
      if (reads <= 4) return 250;
      return reads <= 8 ? 1000 : 2000;
    },
  });
  const cancelRun = useMutation({
    mutationFn: (runId: string) => cancelPlannerQuery(scope, runId),
    onSuccess: () => {
      void runStatus.refetch();
    },
  });
  const runResult = runStatus.data ?? planner.data;
  const run =
    runResult?.outcome === 'envelope' && runResult.envelope.payload.request.query === submitted
      ? runResult.envelope.payload
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
    '/api/plugins/holographic/?limit=25',
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
          // A run is created with a POST, and the project gateway refuses
          // writes for every project but the active one. The gate is read
          // here, before any request, so a refused scope is rendered from the
          // scope authority's own reason and nothing is asked of a daemon that
          // would have to say no.
          const refused = laneFromScope(spec.id, writability);
          if (refused !== null) return refused;
          return searchLane(spec.id, runResult, submitted, terms);
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
      runResult,
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

  const dispatch = (nextQuery: string) => {
    setActiveRunId(null);
    // A fresh run starts at the top of the backoff ladder: the previous
    // run's failures are not this one's, and must not slow its first reads.
    transportFailures.current = 0;
    planner.reset();
    // Nothing is dispatched under a scope that will refuse it; the lanes
    // render the refusal instead.
    if (writability.state === 'writable') planner.mutate(nextQuery);
  };

  // The rows on screen must belong to the scope in the register. When the
  // scope changes under a submitted query, a project selected, a deep link
  // resolving from `unresolved` to `active`, the run is re-created against
  // the new scope rather than left showing the old project's answer.
  const requestScope = `${scopeKey(scope)}:${writability.state}`;
  const lastRequestScope = useRef(requestScope);
  useEffect(() => {
    if (lastRequestScope.current === requestScope) return;
    lastRequestScope.current = requestScope;
    if (submitted === '') return;
    setSelected(null);
    setPeeked(null);
    dispatch(submitted);
    // `dispatch` closes over the current planner and writability; the scope
    // key is the only trigger this effect is meant to have.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [requestScope]);

  const reset = () => {
    setQuery('');
    setSubmitted('');
    setActiveRunId(null);
    transportFailures.current = 0;
    planner.reset();
    setFacet(null);
    setSelected(null);
    setPeeked(null);
  };

  const runProgress: RunProgress | null =
    run === undefined || run.sources.length === 0
      ? null
      : {
          concluded: run.sources.filter((source) => source.outcome !== 'pending').length,
          total: run.sources.length,
        };

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
    // re-derived from the coordinator's own unit accounting rather than
    // reprinted from the `finality` scalar. See `absence.ts`.
    absence: absenceVerdict(run),
    runResult,
    run,
    runProgress,
    cancelling: cancelRun.isPending,
    cancel:
      activeRunId !== null && run?.state === 'pending'
        ? () => cancelRun.mutate(activeRunId)
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
    select: setSelected,
    peek: setPeeked,
  };
}

/* ------------------------------------------------------- session inspector */

export interface ExplorerSessionContext {
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
