/**
 * Explorer's orchestration: every route it calls, every piece of query state it
 * holds, and the typed lane read models it hands to the views.
 *
 * Kept apart from the JSX so the surface has exactly one place that knows a
 * coordinator run is created, polled, and cancelled, and so the views below it
 * render a lane condition rather than deciding one.
 */
import { useMutation, useQuery } from '@tanstack/react-query';
import { useMemo, useState } from 'react';
import {
  ExplorerQueryRunV1Schema,
  ExplorerReadContextV1Schema,
  ExplorerSessionSizeV1Schema,
  GraphOverviewPayloadV1Schema,
  LcmOverviewPayloadV1Schema,
  MemoryOverviewPayloadV1Schema,
  type ExplorerQueryRunV1,
  type ExplorerReadContextV1,
  type ExplorerSessionSizeV1,
} from '../../contracts/generated.ts';
import { fetchEnvelope, type EnvelopeResult } from '../../data/query/envelope.ts';
import { useEnvelope } from '../../data/query/useEnvelope.ts';
import { queryTerms } from '../../ui/search/terms.ts';
import { absenceVerdict, type AbsenceVerdict } from './absence.ts';
import {
  browseLane,
  laneAnswered,
  laneConcluded,
  laneHits,
  lanePending,
  runIsTerminal,
  searchLane,
  type ExplorerLaneReadModel,
} from './laneModel.ts';
import { LANES, type Hit, type LaneId } from './model.ts';

/* ------------------------------------------------------------------ routes */

function createPlannerQuery(query: string): Promise<EnvelopeResult<ExplorerQueryRunV1>> {
  return fetchEnvelope('/api/explorer/queries', ExplorerQueryRunV1Schema, {
    method: 'POST',
    headers: {
      accept: 'application/json',
      'content-type': 'application/json',
    },
    body: JSON.stringify({ query, limit: 50, offset: 0 }),
  });
}

function readPlannerQuery(runId: string): Promise<EnvelopeResult<ExplorerQueryRunV1>> {
  return fetchEnvelope(
    `/api/explorer/queries/${encodeURIComponent(runId)}`,
    ExplorerQueryRunV1Schema,
  );
}

function cancelPlannerQuery(runId: string): Promise<EnvelopeResult<ExplorerQueryRunV1>> {
  return fetchEnvelope(
    `/api/explorer/queries/${encodeURIComponent(runId)}`,
    ExplorerQueryRunV1Schema,
    { method: 'DELETE' },
  );
}

function readSessionSize(
  sessionId: string,
): Promise<EnvelopeResult<ExplorerSessionSizeV1>> {
  return fetchEnvelope(
    `/api/explorer/sessions/${encodeURIComponent(sessionId)}/size`,
    ExplorerSessionSizeV1Schema,
  );
}

function readSessionContext(
  sessionId: string,
): Promise<EnvelopeResult<ExplorerReadContextV1>> {
  return fetchEnvelope(
    `/api/explorer/sessions/${encodeURIComponent(sessionId)}/read-context?limit=25&offset=0&order=asc`,
    ExplorerReadContextV1Schema,
  );
}

/* -------------------------------------------------------------- controller */

export interface ExplorerFacet {
  readonly lane: LaneId;
  readonly value: string;
}

export interface ExplorerController {
  readonly query: string;
  readonly submitted: string;
  readonly searching: boolean;
  readonly terms: readonly string[];
  /** One read model per lane, in `LANES` order. */
  readonly lanes: readonly ExplorerLaneReadModel[];
  readonly laneById: ReadonlyMap<LaneId, ExplorerLaneReadModel>;
  /** The lanes the current filter admits. */
  readonly visibleLanes: readonly ExplorerLaneReadModel[];
  /** Rows from the visible lanes that answered, after the pivot. */
  readonly hits: Hit[];
  readonly anyPending: boolean;
  /** Lanes that neither answered nor are still working. */
  readonly unansweredLanes: readonly ExplorerLaneReadModel[];
  readonly answeredLaneCount: number;
  readonly absence: AbsenceVerdict;
  readonly runResult: EnvelopeResult<ExplorerQueryRunV1> | undefined;
  /** The coordinator run for the submitted query, when one has answered. */
  readonly run: ExplorerQueryRunV1 | undefined;
  readonly cancelling: boolean;
  /** Present only while a cancellable run is in flight. */
  readonly cancel: (() => void) | undefined;
  readonly laneFilter: LaneId | null;
  readonly facet: ExplorerFacet | null;
  readonly selected: Hit | null;
  readonly setQuery: (value: string) => void;
  readonly submit: () => void;
  readonly reset: () => void;
  readonly toggleLaneFilter: (lane: LaneId) => void;
  readonly setFacet: (facet: ExplorerFacet | null) => void;
  readonly select: (hit: Hit | null) => void;
}

export function useExplorerController(): ExplorerController {
  const [query, setQuery] = useState('');
  const [submitted, setSubmitted] = useState('');
  const [activeRunId, setActiveRunId] = useState<string | null>(null);
  const [laneFilter, setLaneFilter] = useState<LaneId | null>(null);
  const [facet, setFacet] = useState<ExplorerFacet | null>(null);
  const [selected, setSelected] = useState<Hit | null>(null);
  const searching = submitted !== '';
  const terms = useMemo(() => queryTerms(submitted), [submitted]);

  const planner = useMutation({
    mutationFn: createPlannerQuery,
    onSuccess: (result) => {
      if (result.outcome === 'envelope') {
        setActiveRunId(result.envelope.payload.run_id);
      }
    },
  });
  const activeRunIdForQuery = activeRunId ?? '';
  const runStatus = useQuery({
    queryKey: ['explorer', 'query-run', activeRunIdForQuery],
    queryFn: () => readPlannerQuery(activeRunIdForQuery),
    enabled: activeRunIdForQuery !== '',
    refetchInterval: (queryState) => {
      const result = queryState.state.data;
      if (result?.outcome !== 'envelope') return 250;
      return runIsTerminal(result.envelope.payload.state) ? false : 250;
    },
  });
  const cancelRun = useMutation({
    mutationFn: cancelPlannerQuery,
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

  const lanes = useMemo<readonly ExplorerLaneReadModel[]>(() => {
    if (searching) {
      return LANES.map((spec) => searchLane(spec.id, runResult, submitted, terms));
    }
    return [
      browseLane('code', graphBrowseData, graphBrowsePending, (data) => data.top_connected, terms),
      browseLane(
        'sessions',
        lcmBrowseData,
        lcmBrowsePending,
        (data) => data.latest_summary_nodes,
        terms,
      ),
      browseLane('knowledge', memoryData, memoryPending, (data) => data.holographic.facts, terms),
    ];
  }, [
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
  ]);

  const laneById = useMemo(
    () => new Map(lanes.map((lane) => [lane.lane, lane])),
    [lanes],
  );
  const { visibleLanes, hits } = useMemo(() => {
    const visibleLanes = laneFilter ? lanes.filter((lane) => lane.lane === laneFilter) : lanes;
    const laneRows = visibleLanes.flatMap((lane) => laneHits(lane));
    const hits = facet
      ? laneRows.filter((hit) => hit.lane === facet.lane && hit.facet === facet.value)
      : laneRows;
    return { visibleLanes, hits };
  }, [facet, laneFilter, lanes]);

  const reset = () => {
    setQuery('');
    setSubmitted('');
    setActiveRunId(null);
    planner.reset();
    setFacet(null);
    setSelected(null);
  };

  return {
    query,
    submitted,
    searching,
    terms,
    lanes,
    laneById,
    visibleLanes,
    hits,
    anyPending: lanes.some((lane) => lanePending(lane)),
    // A typed-absent lane concluded truthfully ("this store does not exist"),
    // so it is neither unanswered nor a reason to withhold zero-result claims.
    unansweredLanes: lanes.filter((lane) => !laneConcluded(lane) && !lanePending(lane)),
    answeredLaneCount: lanes.filter((lane) => laneAnswered(lane)).length,
    // A confirmed global absence is a claim about the whole index, so it is
    // re-derived from the coordinator's own unit accounting rather than
    // reprinted from the `finality` scalar. See `absence.ts`.
    absence: absenceVerdict(run),
    runResult,
    run,
    cancelling: cancelRun.isPending,
    cancel:
      activeRunId !== null && run?.state === 'pending'
        ? () => cancelRun.mutate(activeRunId)
        : undefined,
    laneFilter,
    facet,
    selected,
    setQuery,
    submit: () => {
      const nextQuery = query.trim();
      if (nextQuery === '') return;
      setSubmitted(nextQuery);
      setActiveRunId(null);
      planner.reset();
      planner.mutate(nextQuery);
      setFacet(null);
      setSelected(null);
    },
    reset,
    toggleLaneFilter: (lane) => {
      setLaneFilter((current) => (current === lane ? null : lane));
      setFacet(null);
    },
    setFacet,
    select: setSelected,
  };
}

/* ------------------------------------------------------- session inspector */

export interface ExplorerSessionContext {
  readonly size: EnvelopeResult<ExplorerSessionSizeV1> | undefined;
  readonly readContext: EnvelopeResult<ExplorerReadContextV1> | undefined;
  readonly pending: boolean;
}

/** The two session reads the inspector shows for a transcript row. */
export function useExplorerSessionContext(sessionId: string | undefined): ExplorerSessionContext {
  const sessionIdForQuery = sessionId ?? '';
  const size = useQuery({
    queryKey: ['explorer', 'session-size', sessionIdForQuery],
    queryFn: () => readSessionSize(sessionIdForQuery),
    enabled: sessionIdForQuery !== '',
  });
  const readContext = useQuery({
    queryKey: ['explorer', 'read-context', sessionIdForQuery],
    queryFn: () => readSessionContext(sessionIdForQuery),
    enabled: sessionIdForQuery !== '',
  });
  return {
    size: size.data,
    readContext: readContext.data,
    pending: size.isPending || readContext.isPending,
  };
}
