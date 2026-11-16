/** Explorer source reads and abort ownership, apart from lane presentation. */
import { useQuery } from '@tanstack/react-query';
import { useEffect, useMemo, useRef, useState } from 'react';
import { z } from 'zod';
import {
  DashboardEnvelopeV1Schema,
  ExplorerQueryEventV1Schema,
  ExplorerReadContextV1Schema,
  ExplorerSessionSizeV1Schema,
  GraphOverviewPayloadV1Schema,
  LcmOverviewPayloadV1Schema,
  MemoryOverviewPayloadV1Schema,
  type DashboardDomainStateV1,
  type ExplorerQueryEventV1,
  type ExplorerReadContextV1,
  type ExplorerSessionSizeV1,
} from '../../contracts/generated.ts';
import { dashboardFetch } from '../../data/auth/credential.ts';
import { fetchEnvelope, type EnvelopeResult } from '../../data/query/envelope.ts';
import { decodeJsonBody, isRecord } from '../../data/query/responseBody.ts';
import { useEnvelope } from '../../data/query/useEnvelope.ts';
import {
  scopeKey,
  explorerReadScope,
  scopedUrl,
  useScope,
  type DashboardScope,
} from '../../data/scope/store.ts';
import { queryTerms } from '../../ui/search/terms.ts';
import { absenceVerdict, type AbsenceVerdict } from './absence.ts';
import {
  browseLane,
  laneAnswered,
  laneFromScope,
  laneHits,
  lanePending,
  searchLane,
  type ExplorerLaneReadModel,
  type ExplorerStreamSnapshot,
} from './laneModel.ts';
import { LANES, type Hit, type LaneId } from './model.ts';

/* ------------------------------------------------------------------ routes */

const QUERIES_ROUTE = '/api/explorer/queries';

class ExplorerHttpError extends Error {
  constructor(
    readonly state: DashboardDomainStateV1,
    status: number,
    detail?: string,
  ) {
    super(detail ?? `HTTP ${status}`);
  }
}

/** The daemon's own sentence from a non-envelope problem body, so a refusal
 * keeps its exact reason instead of collapsing to the HTTP status. */
function problemDetail(body: unknown): string | undefined {
  if (!isRecord(body)) return undefined;
  const detail = body.detail;
  return typeof detail === 'string' && detail !== '' ? detail : undefined;
}

export async function readExplorerStream(
  scope: DashboardScope,
  query: string,
  signal: AbortSignal,
  onEvent: (event: ExplorerQueryEventV1) => void,
): Promise<void> {
  const response = await dashboardFetch(scopedUrl(scope, QUERIES_ROUTE), {
    method: 'POST',
    headers: {
      accept: 'text/event-stream',
      'content-type': 'application/json',
    },
    body: JSON.stringify({ query, limit: 50, offset: 0 }),
    signal,
  });
  const contentType = response.headers.get('content-type') ?? '';
  let body: unknown;
  if (contentType.includes('application/json')) {
    body = await decodeJsonBody(response, signal);
    const parsed = DashboardEnvelopeV1Schema(z.unknown()).safeParse(body);
    if (parsed.success && parsed.data.domain_state !== 'ready') {
      throw new ExplorerHttpError(
        parsed.data.domain_state,
        response.status,
        parsed.data.coverage.omission_reasons[0],
      );
    }
  }
  if (!response.ok) {
    throw new ExplorerHttpError(
      response.status === 401 ? 'unauthorized' : response.status === 403 ? 'denied' : 'error',
      response.status,
      problemDetail(body),
    );
  }
  if (!contentType.startsWith('text/event-stream')) {
    throw new ExplorerHttpError('unsupported_schema', response.status, 'Explorer response is not an event stream');
  }
  if (!response.body) throw new Error('Explorer response has no stream');
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = '';
  let final = false;
  let failure: unknown;
  try {
    while (!final) {
      const { done, value } = await reader.read();
      if (done) break;
      buffer = (buffer + decoder.decode(value, { stream: true })).replaceAll('\r\n', '\n');
      let boundary = buffer.indexOf('\n\n');
      while (boundary !== -1) {
        const frame = buffer.slice(0, boundary);
        buffer = buffer.slice(boundary + 2);
        const data = frame.split('\n')
          .filter((line) => line.startsWith('data:'))
          .map((line) => line.slice(5).trimStart())
          .join('\n');
        if (data !== '') {
          const event = ExplorerQueryEventV1Schema.parse(JSON.parse(data));
          onEvent(event);
          if (event.kind === 'final') {
            final = true;
            break;
          }
        }
        boundary = buffer.indexOf('\n\n');
      }
    }
    if (!final && !signal.aborted) throw new Error('Explorer stream ended without a final frame');
  } catch (error) {
    failure = error;
  }
  try {
    await reader.cancel();
  } catch (error) {
    if (error !== failure) {
      console.warn('Explorer stream cleanup failed', error);
    }
  } finally {
    reader.releaseLock();
  }
  if (failure !== undefined) throw failure;
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

/* -------------------------------------------------------------- controller */

interface ExplorerFacet {
  readonly lane: LaneId;
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
  /** Whether the current scope accepts an Explorer read, from the scope authority.
   * Anything but `writable` means no request is dispatched. */
  readonly writability: ReturnType<typeof explorerReadScope>;
  /** One read model per lane, in `LANES` order. */
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
  readonly transport: { state: DashboardDomainStateV1; detail?: string } | undefined;
  readonly run: ExplorerStreamSnapshot | undefined;
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
  const writability = useMemo(() => explorerReadScope(scope), [scope]);
  const [query, setQuery] = useState('');
  const [submitted, setSubmitted] = useState('');
  const [run, setRun] = useState<ExplorerStreamSnapshot>();
  const [transport, setTransport] = useState<{ state: DashboardDomainStateV1; detail?: string }>();
  const [cancelling, setCancelling] = useState(false);
  const [laneFilter, setLaneFilter] = useState<LaneId | null>(null);
  const [facet, setFacet] = useState<ExplorerFacet | null>(null);
  const [selected, setSelected] = useState<Hit | null>(null);
  const [peeked, setPeeked] = useState<Hit | null>(null);
  const searching = submitted !== '';
  const terms = useMemo(() => queryTerms(submitted), [submitted]);
  const requestGeneration = useRef(0);
  const activeRequest = useRef<AbortController | null>(null);
  const retireRequest = () => {
    activeRequest.current?.abort();
    activeRequest.current = null;
  };
  useEffect(() => {
    return () => {
      requestGeneration.current += 1;
      activeRequest.current?.abort();
      activeRequest.current = null;
    };
  }, []);

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
        if (searching) {
          const refused = laneFromScope(spec.id, writability);
          if (refused !== null) return refused;
          return searchLane(spec.id, run, transport, submitted, terms);
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
      run,
      transport,
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
    requestGeneration.current += 1;
    retireRequest();
    setRun(undefined);
    setTransport(undefined);
    setCancelling(false);
    if (writability.state !== 'writable') return;
    const generation = requestGeneration.current;
    const controller = new AbortController();
    activeRequest.current = controller;
    setRun({ query: nextQuery, state: 'pending', elapsed_micros: null, sources: [] });
    void readExplorerStream(scope, nextQuery, controller.signal, (event) => {
      if (controller.signal.aborted || generation !== requestGeneration.current) return;
      setRun((current) => {
        if (
          controller.signal.aborted ||
          generation !== requestGeneration.current ||
          current?.query !== nextQuery
        ) return current;
        if (event.kind === 'final') {
          return { ...current, state: event.state, elapsed_micros: event.elapsed_micros };
        }
        return {
          ...current,
          sources: [
            ...current.sources.filter((source) => source.source_id !== event.source.source_id),
            event.source,
          ],
        };
      });
    }).catch((error: unknown) => {
      if (controller.signal.aborted || generation !== requestGeneration.current) return;
      setTransport({
        state: error instanceof ExplorerHttpError ? error.state
          : error instanceof TypeError ? 'offline' : 'error',
        detail: error instanceof Error ? error.message : 'Explorer stream failed',
      });
    }).finally(() => {
      if (activeRequest.current === controller) activeRequest.current = null;
    });
  };

  const requestScope = `${scopeKey(scope)}:${writability.state}`;
  const lastRequestScope = useRef(requestScope);
  useEffect(() => {
    if (lastRequestScope.current === requestScope) return;
    lastRequestScope.current = requestScope;
    if (submitted === '') return;
    setSelected(null);
    setPeeked(null);
    dispatch(submitted);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [requestScope]);

  const reset = () => {
    requestGeneration.current += 1;
    retireRequest();
    setQuery('');
    setSubmitted('');
    setRun(undefined);
    setTransport(undefined);
    setCancelling(false);
    setFacet(null);
    setSelected(null);
    setPeeked(null);
  };

  const runProgress: RunProgress | null =
    run === undefined
      ? null
      : {
          concluded: run.sources.length,
          total: LANES.length,
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
    absence: absenceVerdict(run),
    transport,
    run,
    runProgress,
    cancelling,
    cancel: activeRequest.current !== null && run?.state === 'pending'
      ? () => {
          requestGeneration.current += 1;
          retireRequest();
          setCancelling(true);
          setRun((current) => current ? { ...current, state: 'cancelled' } : current);
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
    select: setSelected,
    peek: setPeeked,
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
