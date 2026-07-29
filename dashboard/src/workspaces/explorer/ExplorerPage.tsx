import { useMutation, useQuery } from '@tanstack/react-query';
import { useMemo, useState } from 'react';
import { Boxes, Lightbulb, MessagesSquare, type LucideIcon } from 'lucide-react';
import { z } from 'zod';
import {
  DataRow,
  ExplorerSplit,
  InspectorPanel,
  ListCaption,
  RawFields,
  RESULT_ROW_HEIGHT,
} from '../../ui/archetypes/ExplorerSplit.tsx';
import { StateChip, type DomainStateKind } from '../../ui/StateChip';
import { VirtualList } from '../../ui/VirtualList.tsx';
import { Highlight, MetaLabel } from '../../ui/search/Highlight.tsx';
import { FacetGroup } from '../../ui/search/Facets.tsx';
import { SearchField } from '../../ui/search/SearchField.tsx';
import { queryTerms } from '../../ui/search/terms.ts';
import { cn } from '../../ui/cn';
import { Meter } from '../../ui/instrument.tsx';
import { EvidencePattern, type EvidenceQuality } from '../../ui/EvidencePattern.tsx';
import { absenceVerdict, type AbsenceVerdict } from './absence.ts';
import { Reveal } from './Reveal.tsx';
import { AnyObject, type LegacyResult } from '../../data/query/legacy.ts';
import { fetchEnvelope, type EnvelopeResult } from '../../data/query/envelope.ts';
import { useLegacy } from '../../data/query/useLegacy.ts';
import {
  type ExplorerQueryRun,
  ExplorerQueryRunSchema,
  type ExplorerReadContext,
  ExplorerReadContextSchema,
  type ExplorerSessionSize,
  ExplorerSessionSizeSchema,
  type ExplorerSourceId,
  type ExplorerSourceProgress,
} from '../../contracts/wire.ts';
import {
  LANES,
  codeHits,
  facetCounts,
  knowledgeHits,
  plannerLaneState,
  relativeTime,
  sessionHits,
  type Hit,
  type LaneId,
  type LaneSpec,
  type PlannerLaneState,
} from './model.ts';

/* ---------------------------------------------------------------- payloads */

const GraphOverviewPayload = z
  .object({ top_connected: z.array(AnyObject) })
  .passthrough();
const LcmOverviewPayload = z
  .object({
    latest_summary_nodes: z.array(AnyObject),
    overview: z.object({ messages_total: z.number().optional() }).passthrough().optional(),
  })
  .passthrough();
const MemoryPayload = z
  .object({
    holographic: z
      .object({
        facts: z.array(AnyObject),
        overview: z.object({ facts: z.number().optional() }).passthrough().optional(),
      })
      .passthrough(),
  })
  .passthrough();

const LANE_ICON: Record<LaneId, LucideIcon> = {
  code: Boxes,
  sessions: MessagesSquare,
  knowledge: Lightbulb,
};

function laneSpec(id: LaneId): LaneSpec {
  const spec = LANES.find((lane) => lane.id === id);
  if (!spec) throw new Error(`Missing lane specification: ${id}`);
  return spec;
}

const LANE_BY_ID: Record<LaneId, LaneSpec> = {
  code: laneSpec('code'),
  sessions: laneSpec('sessions'),
  knowledge: laneSpec('knowledge'),
};

interface Lane {
  readonly id: LaneId;
  readonly hits: Hit[];
  readonly pending: boolean;
  readonly outcome: LegacyResult<unknown>['outcome'] | 'pending' | 'unknown';
  /** Size of the matching set the daemon reports, when it reports one. */
  readonly reportedTotal?: number | undefined;
}

function queryIsTerminal(state: ExplorerQueryRun['state']): boolean {
  switch (state) {
    case 'pending':
      return false;
    case 'completed':
    case 'partial':
    case 'cancelled':
    case 'error':
      return true;
    default: {
      const exhaustive: never = state;
      return exhaustive;
    }
  }
}

function createPlannerQuery(query: string): Promise<EnvelopeResult<ExplorerQueryRun>> {
  return fetchEnvelope('/api/explorer/queries', ExplorerQueryRunSchema, {
    method: 'POST',
    headers: {
      accept: 'application/json',
      'content-type': 'application/json',
    },
    body: JSON.stringify({ query, limit: 50, offset: 0 }),
  });
}

function readPlannerQuery(runId: string): Promise<EnvelopeResult<ExplorerQueryRun>> {
  return fetchEnvelope(`/api/explorer/queries/${encodeURIComponent(runId)}`, ExplorerQueryRunSchema);
}

function cancelPlannerQuery(runId: string): Promise<EnvelopeResult<ExplorerQueryRun>> {
  return fetchEnvelope(`/api/explorer/queries/${encodeURIComponent(runId)}`, ExplorerQueryRunSchema, {
    method: 'DELETE',
  });
}

function readSessionSize(sessionId: string): Promise<EnvelopeResult<ExplorerSessionSize>> {
  return fetchEnvelope(
    `/api/explorer/sessions/${encodeURIComponent(sessionId)}/size`,
    ExplorerSessionSizeSchema,
  );
}

function readSessionContext(sessionId: string): Promise<EnvelopeResult<ExplorerReadContext>> {
  return fetchEnvelope(
    `/api/explorer/sessions/${encodeURIComponent(sessionId)}/read-context?limit=25&offset=0&order=asc`,
    ExplorerReadContextSchema,
  );
}

function plannerStateKind(state: ExplorerQueryRun['state']): DomainStateKind {
  switch (state) {
    case 'pending':
      return 'loading';
    case 'completed':
      return 'ready';
    case 'partial':
      return 'partial';
    case 'cancelled':
      return 'cancelled';
    case 'error':
      return 'error';
    default: {
      const exhaustive: never = state;
      return exhaustive;
    }
  }
}

/**
 * Where a lane's quantity sits on the shared evidence axis.
 *
 * `measured` is reserved for a source that answered AND reported the size of
 * the matching set — the only case in which the number on screen has a real
 * denominator behind it. Rows without a reported total are `associated`: they
 * are genuine rows, but the surface cannot say what fraction of the truth they
 * are. A lane that did not answer, or has not answered yet, is `unknown`.
 * `predicted` is deliberately unreachable: Explorer never estimates.
 */
function laneEvidence(lane: Lane | undefined): EvidenceQuality {
  if (lane === undefined || lane.pending || lane.outcome !== 'ok') return 'unknown';
  return lane.reportedTotal != null ? 'measured' : 'associated';
}

/**
 * The state a browse lane reports for itself.
 *
 * Exhaustive, because a browse lane's outcome comes straight off `fetchLegacy`
 * and so inherits every reading that helper can produce. It used to be a
 * `=== 'offline' ? 'offline' : 'error'` ternary, which meant an authorization
 * refusal on one memory read as that memory being broken.
 */
function laneStateKind(outcome: Lane['outcome']): DomainStateKind {
  switch (outcome) {
    case 'ok':
      return 'ready';
    case 'pending':
      return 'loading';
    case 'unknown':
      return 'unknown';
    case 'offline':
      return 'offline';
    case 'unauthorized':
      return 'unauthorized';
    case 'denied':
      return 'denied';
    case 'unsupported_schema':
      return 'unsupported_schema';
    case 'error':
      return 'error';
    default: {
      const exhaustive: never = outcome;
      return exhaustive;
    }
  }
}

function sourceStateKind(outcome: ExplorerSourceProgress['outcome']): DomainStateKind {
  switch (outcome) {
    case 'pending':
      return 'loading';
    case 'ready':
      return 'ready';
    case 'unavailable':
      return 'offline';
    case 'error':
      return 'error';
    case 'cancelled':
      return 'cancelled';
    default: {
      const exhaustive: never = outcome;
      return exhaustive;
    }
  }
}

/**
 * Explorer — one query, three memories.
 *
 * TraceDecay remembers a repository three separate ways, and the honest
 * consequence is that a search is a *fan-out*, not a single ranked list: the
 * code graph, the transcript store, and the fact store each answer for
 * themselves. This surface makes that structure the design. Each memory is a
 * lane with its own identity rail and its own live state; results stay
 * comparable through one row grammar; and because the daemon returns hits but
 * no relevance score, "why this is here" is told with things that are actually
 * true — the daemon's own ordering, the fields whose text really contains the
 * term, and measured quantities (graph degree, fact trust) that always name
 * the field they came from.
 *
 * Before a query, the surface is not blank: it browses what each memory holds
 * right now, from the same endpoints' overview shapes.
 */
export function ExplorerPage() {
  const [query, setQuery] = useState('');
  const [submitted, setSubmitted] = useState('');
  const [activeRunId, setActiveRunId] = useState<string | null>(null);
  const [laneFilter, setLaneFilter] = useState<LaneId | null>(null);
  const [facet, setFacet] = useState<{ lane: LaneId; value: string } | null>(null);
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
      return queryIsTerminal(result.envelope.payload.state) ? false : 250;
    },
  });
  const cancelRun = useMutation({
    mutationFn: cancelPlannerQuery,
    onSuccess: () => {
      void runStatus.refetch();
    },
  });
  const plannerResult = runStatus.data ?? planner.data;
  const plannerRun =
    plannerResult?.outcome === 'envelope' &&
    plannerResult.envelope.payload.request.query === submitted
      ? plannerResult.envelope.payload
      : undefined;

  const graphBrowse = useLegacy(
    ['explorer', 'graph-overview'],
    '/api/plugins/graph/overview',
    GraphOverviewPayload,
    { enabled: !searching },
  );
  const lcmBrowse = useLegacy(
    ['explorer', 'lcm-overview'],
    '/api/plugins/hermes-lcm/overview',
    LcmOverviewPayload,
    { enabled: !searching },
  );
  const memory = useLegacy(
    ['explorer', 'memory-overview'],
    '/api/plugins/holographic/?limit=25',
    MemoryPayload,
    { enabled: !searching },
  );
  const graphBrowseData = graphBrowse.data;
  const graphBrowsePending = graphBrowse.isPending;
  const lcmBrowseData = lcmBrowse.data;
  const lcmBrowsePending = lcmBrowse.isPending;
  const memoryData = memory.data;
  const memoryPending = memory.isPending;

  const lanes: Lane[] = useMemo(() => {
    if (searching) {
      const fallbackOutcome =
        plannerResult?.outcome === 'transport'
          ? plannerResult.state === 'offline'
            ? ('offline' as const)
            : ('error' as const)
          : ('pending' as const);
      const stateFor = (sourceId: ExplorerSourceId): PlannerLaneState => {
        const source = plannerRun?.sources.find((candidate) => candidate.source_id === sourceId);
        return source
          ? plannerLaneState(source, sourceId)
          : { pending: fallbackOutcome === 'pending', outcome: fallbackOutcome, rows: [] };
      };
      const code = stateFor('code_graph');
      const sessions = stateFor('sessions');
      const knowledge = stateFor('knowledge');
      return [
        {
          id: 'code' as const,
          hits: codeHits(code.rows, terms),
          pending: code.pending,
          outcome: code.outcome,
          ...('reportedTotal' in code ? { reportedTotal: code.reportedTotal } : {}),
        },
        {
          id: 'sessions' as const,
          hits: sessionHits(sessions.rows, terms),
          pending: sessions.pending,
          outcome: sessions.outcome,
          ...('reportedTotal' in sessions
            ? { reportedTotal: sessions.reportedTotal }
            : {}),
        },
        {
          id: 'knowledge' as const,
          hits: knowledgeHits(knowledge.rows, terms),
          pending: knowledge.pending,
          outcome: knowledge.outcome,
          ...('reportedTotal' in knowledge
            ? { reportedTotal: knowledge.reportedTotal }
            : {}),
        },
      ];
    }
    const codeRows =
      graphBrowseData?.outcome === 'ok'
        ? (graphBrowseData.data as z.infer<typeof GraphOverviewPayload>).top_connected
        : [];
    const sessionRows =
      lcmBrowseData?.outcome === 'ok'
        ? (lcmBrowseData.data as z.infer<typeof LcmOverviewPayload>).latest_summary_nodes
        : [];
    const factRows =
      memoryData?.outcome === 'ok'
        ? (memoryData.data as z.infer<typeof MemoryPayload>).holographic.facts
        : [];
    return [
      {
        id: 'code' as const,
        hits: codeHits(codeRows, terms),
        pending: graphBrowsePending,
        outcome: graphBrowsePending
          ? ('pending' as const)
          : (graphBrowseData?.outcome ?? 'unknown'),
      },
      {
        id: 'sessions' as const,
        hits: sessionHits(sessionRows, terms),
        pending: lcmBrowsePending,
        outcome: lcmBrowsePending
          ? ('pending' as const)
          : (lcmBrowseData?.outcome ?? 'unknown'),
      },
      {
        id: 'knowledge' as const,
        hits: knowledgeHits(factRows, terms),
        pending: memoryPending,
        outcome: memoryPending ? ('pending' as const) : (memoryData?.outcome ?? 'unknown'),
      },
    ];
  }, [
    graphBrowseData,
    graphBrowsePending,
    lcmBrowseData,
    lcmBrowsePending,
    memoryData,
    memoryPending,
    plannerResult,
    plannerRun,
    searching,
    terms,
  ]);

  const laneById = useMemo(
    () => new Map(lanes.map((lane) => [lane.id, lane])),
    [lanes],
  );
  const { visibleLanes, hits } = useMemo(() => {
    const visibleLanes = laneFilter ? lanes.filter((lane) => lane.id === laneFilter) : lanes;
    const laneHits = visibleLanes.flatMap((lane) => lane.hits);
    const hits = facet
      ? laneHits.filter((hit) => hit.lane === facet.lane && hit.facet === facet.value)
      : laneHits;
    return { visibleLanes, hits };
  }, [facet, laneFilter, lanes]);
  const anyPending = lanes.some((lane) => lane.pending);
  const failedLanes = lanes.filter(
    (lane) => lane.outcome !== 'ok' && lane.outcome !== 'pending',
  );
  const answeredLanes = lanes.filter((lane) => lane.outcome === 'ok');
  // A confirmed global absence is a claim about the whole index, so the client
  // re-derives it from the coordinator's own unit accounting rather than
  // reprinting the `finality` scalar as fact. Checking `completeness` alone was
  // not enough: a source could declare complete coverage over a real
  // denominator while its `unknown` and `omitted` counts on the same object said
  // it knew the status of nothing, or had examined nothing. See `absence.ts`.
  const absence = absenceVerdict(plannerRun);

  const reset = () => {
    setQuery('');
    setSubmitted('');
    setActiveRunId(null);
    planner.reset();
    setFacet(null);
    setSelected(null);
  };

  return (
    <ExplorerSplit
      stackOnNarrow
      header={
        <div className="flex shrink-0 flex-col gap-3 border-b border-edge-subtle bg-surface-1 px-4 py-3">
          <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
            <h1 className="text-sm font-semibold tracking-tight">Explorer</h1>
            <p className="text-2xs text-text-muted">
              one coordinator run, three source-local answers
            </p>
          </div>
          <div className="flex min-w-0 flex-col gap-3 lg:flex-row lg:items-start">
            <SearchField
              value={query}
              onChange={setQuery}
              onSubmit={() => {
                const nextQuery = query.trim();
                if (nextQuery === '') return;
                setSubmitted(nextQuery);
                setActiveRunId(null);
                planner.reset();
                planner.mutate(nextQuery);
                setFacet(null);
                setSelected(null);
              }}
              onClear={reset}
              submitted={submitted}
              label="Search code, sessions, and knowledge"
              placeholder="Search everything the daemon remembers…"
              hint={
                searching ? (
                  <>
                    showing hits for{' '}
                    <span className="font-medium text-text-secondary">“{submitted}”</span> in the
                    order returned by each source · terms are marked where they occur in the
                    payload
                  </>
                ) : (
                  <>
                    quote a phrase to keep it whole · press{' '}
                    <kbd className="rounded-[var(--radius-chip)] border border-edge-subtle px-1">
                      /
                    </kbd>{' '}
                    to focus, <kbd className="rounded-[var(--radius-chip)] border border-edge-subtle px-1">Esc</kbd>{' '}
                    to return to browsing
                  </>
                )
              }
            />
            <div
              className="flex shrink-0 flex-wrap gap-2"
              aria-label="Memory lanes"
              role="group"
            >
              {LANES.map((spec, laneIndex) => (
                <Reveal
                  key={spec.id}
                  index={laneIndex}
                  // Three readouts abreast inside a 320px column leaves ~35px
                  // for the label, which clipped every lane name to "CODE …".
                  // Two per row below `lg` gives the name its full measure; the
                  // third stretches across the next row rather than sitting in
                  // a ragged third of one.
                  className="min-w-0 flex-1 basis-[calc(50%-0.25rem)] lg:basis-auto lg:flex-none"
                >
                  <LaneReadout
                    spec={spec}
                    lane={laneById.get(spec.id)}
                    searching={searching}
                    active={laneFilter === spec.id}
                    onToggle={() => {
                      setLaneFilter(laneFilter === spec.id ? null : spec.id);
                      setFacet(null);
                    }}
                  />
                </Reveal>
              ))}
            </div>
          </div>
        </div>
      }
      filters={
        <div className="flex flex-col gap-4">
          {searching ? (
            <Reveal index={3}>
              <PlannerRunPanel
                result={plannerResult}
                run={plannerRun}
                cancelling={cancelRun.isPending}
                onCancel={
                  activeRunId && plannerRun?.state === 'pending'
                    ? () => cancelRun.mutate(activeRunId)
                    : undefined
                }
              />
            </Reveal>
          ) : null}
          {visibleLanes.map((lane) => {
            const spec = LANE_BY_ID[lane.id];
            const counts = facetCounts(lane.hits);
            if (counts.length === 0) return null;
            return (
              <FacetGroup
                key={lane.id}
                title={spec.facetLabel}
                note="loaded rows"
                facets={counts}
                active={facet?.lane === lane.id ? facet.value : null}
                onToggle={(value) =>
                  setFacet(value === null ? null : { lane: lane.id, value })
                }
              />
            );
          })}
          <Reveal index={4}>
            <section className="flex flex-col gap-2">
              <MetaLabel>What each lane searches</MetaLabel>
              <dl className="flex flex-col gap-2.5">
                {LANES.map((spec) => {
                  const lane = laneById.get(spec.id);
                  return (
                    <div key={spec.id} className="flex flex-col gap-0.5">
                      <dt className="flex items-center gap-2 text-2xs font-medium text-text-secondary">
                        <span
                          aria-hidden
                          className={cn('h-3 w-[3px] shrink-0 rounded-full', spec.railClass)}
                        />
                        <span>{spec.label}</span>
                      </dt>
                      <dd className="flex flex-col gap-1 pl-[11px] text-2xs leading-relaxed text-text-muted">
                        <span>
                          {searching ? spec.searches : spec.browseLabel}
                          {lane?.reportedTotal != null
                            ? ` · daemon reports ${lane.reportedTotal.toLocaleString()} matching`
                            : ''}
                        </span>
                        {/* How well the quantity above is known, on the shared
                          * evidence axis: solid when the source reported a real
                          * denominator, hatched when rows arrived without one,
                          * dashed when the source never answered. */}
                        <EvidencePattern quality={laneEvidence(lane)} />
                      </dd>
                    </div>
                  );
                })}
              </dl>
            </section>
          </Reveal>
          {failedLanes.length > 0 ? (
            <Reveal index={5}>
              <section className="flex flex-col gap-1.5 border-l-2 border-state-partial pl-2">
                <MetaLabel>Unanswered</MetaLabel>
                {failedLanes.map((lane) => (
                  <p key={lane.id} className="flex items-center gap-1.5 text-2xs text-text-muted">
                    <StateChip kind={laneStateKind(lane.outcome)} />
                    <span>{LANE_BY_ID[lane.id].label}</span>
                  </p>
                ))}
                <p className="text-2xs leading-relaxed text-text-muted">
                  Results are only from the lanes that answered. Nothing is being substituted
                  for the rest, and no count is shown for a lane that reported none.
                </p>
              </section>
            </Reveal>
          ) : null}
        </div>
      }
      list={
        hits.length === 0 ? (
          <EmptyResults
            searching={searching}
            pending={anyPending}
            query={submitted}
            facet={facet?.value ?? null}
            failed={failedLanes.length > 0}
            absence={absence}
            onClearFacet={() => setFacet(null)}
            onClearQuery={reset}
          />
        ) : (
          <VirtualList
            items={hits}
            estimateHeight={RESULT_ROW_HEIGHT}
            getKey={(hit) => hit.key}
            header={
              <ListCaption>
                <span className="td-display text-xs">{hits.length.toLocaleString()}</span>
                <span>
                  {searching ? 'results' : 'rows'}
                  {laneFilter
                    ? ` in ${LANE_BY_ID[laneFilter].label.toLowerCase()}`
                    : // Never claim breadth the run did not deliver: a source
                      // that failed, went unavailable, or is still reading did
                      // not contribute to this set, and saying otherwise would
                      // credit it for rows it never returned.
                      answeredLanes.length === lanes.length
                      ? ` across ${lanes.length} memories`
                      : ` across ${answeredLanes.length} of ${lanes.length} memories`}
                  {facet ? ` · ${facet.value}` : ''}
                </span>
                <span aria-hidden className="ml-auto hidden sm:inline">
                  ordered by each memory&rsquo;s own ranking
                </span>
              </ListCaption>
            }
            renderItem={(hit, index) => (
              <HitRow
                hit={hit}
                terms={terms}
                selected={selected?.key === hit.key}
                // A source-local order is only readable if the seam between two
                // sources is visible; without it seven rows from three
                // independent answers read as one ranked list.
                startsLane={index === 0 || hits[index - 1]?.lane !== hit.lane}
                onSelect={() => setSelected(hit)}
              />
            )}
          />
        )
      }
      inspector={
        selected ? (
          <HitInspector
            hit={selected}
            terms={terms}
            onClose={() => setSelected(null)}
          />
        ) : undefined
      }
    />
  );
}

/**
 * One memory's readout: what it is, how much of it we are holding, and how
 * well that quantity is known.
 *
 * The accessible name is composed entirely from visible text — there is no
 * `aria-label` override — so a screen reader and a sighted reader are told the
 * same sentence, and the visible label can never drift out of the name
 * (WCAG 2.5.3). The quantity is only ever a number the source actually
 * reported: a lane that did not answer shows its state and says so, never a
 * zero, and the proportional rail is drawn only when a real denominator exists.
 */
function LaneReadout({
  spec,
  lane,
  searching,
  active,
  onToggle,
}: {
  spec: LaneSpec;
  lane: Lane | undefined;
  searching: boolean;
  active: boolean;
  onToggle: () => void;
}) {
  const Icon = LANE_ICON[spec.id];
  const answered = lane?.outcome === 'ok';
  const total = lane?.reportedTotal;
  const loaded = lane?.hits.length ?? 0;
  const share = answered && total != null && total > 0 ? loaded / total : null;
  return (
    <button
      type="button"
      aria-pressed={active}
      onClick={onToggle}
      className={cn(
        'flex h-full w-full min-w-[7.5rem] flex-col gap-1.5 border px-2.5 py-2 text-left',
        'rounded-[var(--radius-standard)] transition-colors duration-[var(--dur-state)]',
        'ease-[var(--ease-standard)] motion-reduce:transition-none',
        active
          ? 'border-accent bg-surface-2'
          : 'border-edge-subtle bg-surface-0 hover:border-edge-strong',
      )}
    >
      <span className="flex items-center gap-1.5">
        <span
          aria-hidden
          className={cn('h-2.5 w-[3px] shrink-0 rounded-full', spec.railClass)}
        />
        {/* The coloured rail already carries lane identity, and at 320px the
          * icon plus its gap was the ~18px that pushed "Code graph" into an
          * ellipsis. The name outranks the glyph, so the glyph yields. */}
        <Icon aria-hidden size={12} className={cn('hidden shrink-0 sm:block', spec.textClass)} />
        <span className="td-legend truncate text-text-secondary">{spec.label}</span>
      </span>
      {lane?.pending ? (
        <StateChip kind="loading" detail="reading" />
      ) : answered ? (
        <span className="flex flex-wrap items-baseline gap-x-1.5 gap-y-0.5">
          <span className="td-display text-base">{loaded.toLocaleString()}</span>
          <span className="min-w-0 text-2xs leading-tight text-text-muted">
            {searching
              ? total != null
                ? `loaded of ${total.toLocaleString()} matching rows reported`
                : 'loaded with no source total reported'
              : 'shown from the overview endpoint'}
          </span>
        </span>
      ) : (
        <span className="flex flex-wrap items-center gap-x-1.5 gap-y-0.5">
          <StateChip kind={laneStateKind(lane?.outcome ?? 'unknown')} />
          <span className="text-2xs leading-tight text-text-muted">no count reported</span>
        </span>
      )}
      {share !== null ? (
        <Meter fraction={share} className="w-full rounded-full" tone="bg-accent/80" />
      ) : null}
    </button>
  );
}

function PlannerRunPanel({
  result,
  run,
  cancelling,
  onCancel,
}: {
  result: EnvelopeResult<ExplorerQueryRun> | undefined;
  run: ExplorerQueryRun | undefined;
  cancelling: boolean;
  onCancel?: () => void;
}) {
  if (!run) {
    const state =
      result?.outcome === 'transport'
        ? result.state === 'offline'
          ? 'offline'
          : result.state
        : 'loading';
    return (
      <section className="flex flex-col gap-2" aria-live="polite">
        <MetaLabel>Coordinator run</MetaLabel>
        <StateChip
          kind={state}
          detail={
            result?.outcome === 'transport'
              ? result.detail ?? 'planner response unavailable'
              : 'admitting the source plan'
          }
        />
      </section>
    );
  }
  return (
    <section className="flex flex-col gap-2" aria-live="polite">
      <div className="flex flex-wrap items-center gap-2">
        <MetaLabel>Coordinator run</MetaLabel>
        <StateChip kind={plannerStateKind(run.state)} detail={run.finality} />
      </div>
      <p className="break-all font-mono text-2xs text-text-muted">{run.run_id}</p>
      <p className="text-2xs leading-relaxed text-text-secondary">{run.explanation}</p>
      {/* Revision and policy identifiers are single unbreakable tokens, and a
        * `1fr` track cannot shrink below an unbreakable word: in a 224px rail
        * both ran straight off the panel and read as `explorer-query-plan-`.
        * `min-w-0` lets the track shrink; `break-words` then wraps at the
        * hyphens and underscores the identifiers already contain, rather than
        * slicing mid-word the way `break-all` does. */}
      <dl className="grid grid-cols-[auto_1fr] gap-x-2 gap-y-1 text-2xs">
        <dt className="text-text-muted">Plan</dt>
        <dd className="min-w-0 break-words font-mono text-text-secondary">{run.plan_revision}</dd>
        <dt className="text-text-muted">Ordering</dt>
        <dd className="min-w-0 break-words text-text-secondary">{run.ordering_policy}</dd>
        <dt className="text-text-muted">Elapsed</dt>
        <dd className="td-value text-2xs">
          {Math.round(run.elapsed_micros / 1_000).toLocaleString()} ms
        </dd>
      </dl>
      <ul className="flex flex-col gap-1.5" aria-label="Source progress">
        {run.sources.map((source) => (
          <li
            key={source.source_id}
            className="flex min-w-0 flex-col gap-1 border-l-2 border-edge-strong pl-2"
          >
            <span className="flex flex-wrap items-center gap-1.5">
              <span className="text-2xs font-medium text-text-secondary">
                {source.source_label}
              </span>
              <StateChip kind={sourceStateKind(source.outcome)} detail={source.phase} />
            </span>
            <span className="text-2xs text-text-muted">
              {source.completed_units !== null && source.total_units !== null
                ? `${source.completed_units.toLocaleString()} of ${source.total_units.toLocaleString()} ${source.coverage.unit ?? 'units'}`
                : source.completed_units !== null
                  ? `${source.completed_units.toLocaleString()} loaded · total unknown`
                  : 'denominator unknown'}
            </span>
            {source.message ? (
              <span className="text-2xs leading-relaxed text-text-muted">
                {source.error_code ? `${source.error_code}: ` : ''}
                {source.message}
              </span>
            ) : null}
          </li>
        ))}
      </ul>
      {onCancel ? (
        <button
          type="button"
          onClick={onCancel}
          disabled={cancelling}
          className="flex min-h-[var(--touch-target-min)] items-center justify-center border border-edge-subtle px-3 text-2xs text-text-secondary hover:border-accent hover:text-text-primary disabled:opacity-50"
        >
          {cancelling ? 'Requesting cancellation…' : 'Cancel this run'}
        </button>
      ) : null}
    </section>
  );
}

/* ------------------------------------------------------------------- rows */

function HitRow({
  hit,
  terms,
  selected,
  startsLane,
  onSelect,
}: {
  hit: Hit;
  terms: readonly string[];
  selected: boolean;
  /** First row of a run of rows from the same source. */
  startsLane: boolean;
  onSelect: () => void;
}) {
  const spec = LANE_BY_ID[hit.lane];
  const Icon = LANE_ICON[hit.lane];
  const age = relativeTime(hit.stamp);
  return (
    <DataRow
      selected={selected}
      onSelect={onSelect}
      height={RESULT_ROW_HEIGHT}
      railClassName={spec.railClass}
      className={cn('pl-4', startsLane && 'border-t border-edge-strong')}
    >
      <span className="flex w-10 shrink-0 flex-col items-center gap-0.5">
        <Icon aria-hidden size={13} className={cn(spec.textClass, 'opacity-80')} />
        <span className="td-value text-2xs leading-none text-text-muted">#{hit.rank}</span>
      </span>
      <span className="flex min-w-0 flex-1 flex-col gap-0.5">
        <span className="flex min-w-0 items-baseline gap-2">
          <Highlight
            text={hit.title}
            terms={terms}
            className={cn(
              'min-w-0 flex-1 truncate text-xs text-text-primary',
              hit.lane === 'code' && 'font-mono',
            )}
          />
          {hit.facet ? (
            <span className="shrink-0 rounded-[var(--radius-chip)] border border-edge-subtle px-1.5 text-2xs text-text-secondary">
              {hit.facet}
            </span>
          ) : null}
        </span>
        <span className="flex min-w-0 items-baseline gap-2 text-2xs text-text-muted">
          {hit.context ? (
            <Highlight
              text={hit.context}
              terms={terms}
              className="min-w-0 max-w-[22rem] shrink truncate"
            />
          ) : null}
          {hit.body ? (
            <Highlight text={hit.body} terms={terms} className="min-w-0 flex-1 truncate" />
          ) : null}
          {hit.matchedIn.length > 0 ? (
            <span className="hidden shrink-0 truncate text-text-secondary lg:inline">
              matched in {hit.matchedIn.join(', ')}
            </span>
          ) : null}
        </span>
      </span>
      <span className="hidden w-28 shrink-0 items-center justify-end gap-2 md:flex">
        {hit.signal ? (
          <>
            <span className="tabular text-2xs text-text-muted">{hit.signal.display}</span>
            <Meter
              fraction={hit.signal.max > 0 ? hit.signal.value / hit.signal.max : null}
              className="w-10 rounded-full"
              tone="bg-accent/80"
              ariaLabel={`${hit.signal.field} ${hit.signal.value}`}
            />
          </>
        ) : null}
      </span>
      <span className="tabular w-10 shrink-0 text-right text-2xs text-text-muted">
        {age ?? ''}
      </span>
    </DataRow>
  );
}

/* -------------------------------------------------------------- inspector */

function HitInspector({
  hit,
  terms,
  onClose,
}: {
  hit: Hit;
  terms: readonly string[];
  onClose: () => void;
}) {
  const spec = LANE_BY_ID[hit.lane];
  const Icon = LANE_ICON[hit.lane];
  const age = relativeTime(hit.stamp);
  const rawSessionId = hit.lane === 'sessions' ? hit.raw['session_id'] : undefined;
  const sessionId =
    typeof rawSessionId === 'string' && rawSessionId.trim() !== ''
      ? rawSessionId.trim()
      : undefined;
  const sessionIdForQuery = sessionId ?? '';
  const sessionSize = useQuery({
    queryKey: ['explorer', 'session-size', sessionIdForQuery],
    queryFn: () => readSessionSize(sessionIdForQuery),
    enabled: sessionIdForQuery !== '',
  });
  const readContext = useQuery({
    queryKey: ['explorer', 'read-context', sessionIdForQuery],
    queryFn: () => readSessionContext(sessionIdForQuery),
    enabled: sessionIdForQuery !== '',
  });
  return (
    <InspectorPanel
      title={hit.title}
      eyebrow={
        <>
          <Icon aria-hidden size={11} className={spec.textClass} />
          {spec.label} · rank {hit.rank}
        </>
      }
      onClose={onClose}
    >
      <div className="flex flex-col gap-4">
        <section className="flex flex-col gap-1.5">
          <MetaLabel>Why this is here</MetaLabel>
          <p className="text-2xs leading-relaxed text-text-secondary">
            {terms.length === 0 ? (
              <>
                Browsing {spec.browseLabel}; position {hit.rank} is the order the daemon
                returned, not a score.
              </>
            ) : hit.matchedIn.length > 0 ? (
              <>
                Position {hit.rank} in {hit.orderLabel}. The query text occurs in{' '}
                <span className="font-mono text-text-primary">
                  {hit.matchedIn.join(', ')}
                </span>
                .
              </>
            ) : (
              <>
                Position {hit.rank} in {hit.orderLabel}. The daemon matched on its
                own index; the literal terms do not appear in the fields it returned.
              </>
            )}
          </p>
        </section>
        {hit.context ? (
          <section className="flex flex-col gap-1">
            <MetaLabel>Where</MetaLabel>
            <Highlight
              text={hit.context}
              terms={terms}
              className="break-all font-mono text-2xs text-text-secondary"
            />
          </section>
        ) : null}
        {hit.signal ? (
          <section className="flex flex-col gap-1.5">
            <MetaLabel>Measured</MetaLabel>
            <span className="flex items-center gap-2">
              <Meter
                fraction={hit.signal.max > 0 ? hit.signal.value / hit.signal.max : null}
                className="w-10 rounded-full"
                tone="bg-accent/80"
                ariaLabel={`${hit.signal.field} ${hit.signal.value}`}
              />
              <span className="tabular text-xs text-text-primary">
                {hit.signal.display}
              </span>
              <span className="font-mono text-2xs text-text-muted">
                {hit.signal.field}
              </span>
            </span>
          </section>
        ) : null}
        {hit.body ? (
          <section className="flex flex-col gap-1">
            <MetaLabel>{hit.lane === 'code' ? 'Signature' : 'Body'}</MetaLabel>
            <Highlight
              text={hit.body}
              terms={terms}
              className={cn(
                'whitespace-pre-wrap break-words text-xs leading-relaxed text-text-secondary',
                hit.lane === 'code' && 'font-mono',
              )}
            />
          </section>
        ) : null}
        <section className="flex flex-col gap-1">
          <MetaLabel>{hit.titleField}</MetaLabel>
          <Highlight
            text={hit.title}
            terms={terms}
            className={cn(
              'whitespace-pre-wrap break-words text-xs leading-[1.6] text-text-primary',
              hit.lane === 'code' && 'font-mono',
            )}
          />
          {age ? <span className="text-2xs text-text-muted">{age} ago</span> : null}
        </section>
        {sessionId ? (
          <SessionContextDetails
            sessionId={sessionId}
            size={sessionSize.data}
            readContext={readContext.data}
            pending={sessionSize.isPending || readContext.isPending}
          />
        ) : null}
        <RawFields value={hit.raw} label="Payload provenance" />
      </div>
    </InspectorPanel>
  );
}

function SessionContextDetails({
  sessionId,
  size,
  readContext,
  pending,
}: {
  sessionId: string;
  size: EnvelopeResult<ExplorerSessionSize> | undefined;
  readContext: EnvelopeResult<ExplorerReadContext> | undefined;
  pending: boolean;
}) {
  const sizePayload = size?.outcome === 'envelope' ? size.envelope.payload : undefined;
  const contextPayload =
    readContext?.outcome === 'envelope' ? readContext.envelope.payload : undefined;
  if (pending && !sizePayload && !contextPayload) {
    return (
      <section className="flex flex-col gap-1.5">
        <MetaLabel>Session context</MetaLabel>
        <StateChip kind="loading" detail={sessionId} />
      </section>
    );
  }
  if (!sizePayload && !contextPayload) {
    const offline =
      size?.outcome === 'transport' && size.state === 'offline'
        ? true
        : readContext?.outcome === 'transport' && readContext.state === 'offline';
    return (
      <section className="flex flex-col gap-1.5">
        <MetaLabel>Session context</MetaLabel>
        <StateChip kind={offline ? 'offline' : 'error'} detail={sessionId} />
      </section>
    );
  }
  return (
    <section className="flex flex-col gap-2">
      <MetaLabel>Session context</MetaLabel>
      {sizePayload ? (
        <dl className="grid grid-cols-[auto_1fr] gap-x-2 gap-y-1 text-2xs">
          <dt className="text-text-muted">Messages</dt>
          <dd className="tabular text-text-secondary">
            {sizePayload.counts.message_count.toLocaleString()}
          </dd>
          <dt className="text-text-muted">Summary nodes</dt>
          <dd className="tabular text-text-secondary">
            {sizePayload.counts.summary_node_count.toLocaleString()}
          </dd>
          <dt className="text-text-muted">Raw token estimate</dt>
          <dd className="tabular text-text-secondary">
            {sizePayload.counts.token_estimate_total.toLocaleString()}
          </dd>
          <dt className="text-text-muted">Store</dt>
          <dd className="text-text-secondary">{sizePayload.storage_scope}</dd>
        </dl>
      ) : null}
      {contextPayload ? (
        <>
          <p className="text-2xs leading-relaxed text-text-muted">
            Loaded {contextPayload.messages.length.toLocaleString()} raw messages and{' '}
            {contextPayload.summary_nodes.length.toLocaleString()} summary nodes in{' '}
            {contextPayload.order} order
            {contextPayload.has_more ? '; more rows remain' : '; this read is complete'}.
          </p>
          <RawFields
            value={contextPayload}
            label="Session read context returned by the daemon"
          />
        </>
      ) : null}
    </section>
  );
}

/* ----------------------------------------------------------- empty states */

function EmptyResults({
  searching,
  pending,
  query,
  facet,
  failed,
  absence,
  onClearFacet,
  onClearQuery,
}: {
  searching: boolean;
  pending: boolean;
  query: string;
  facet: string | null;
  failed: boolean;
  /** Whether a global-absence claim has been earned, and if not, the specific
   * thing standing in the way. */
  absence: AbsenceVerdict;
  onClearFacet: () => void;
  onClearQuery: () => void;
}) {
  if (pending) {
    return (
      <div className="flex h-full flex-col items-center justify-center gap-3 p-8 text-center">
        <StateChip kind="loading" />
        <p className="text-2xs text-text-muted">The coordinator is reading required sources.</p>
      </div>
    );
  }
  if (facet) {
    return (
      <div className="flex h-full flex-col items-center justify-center gap-3 p-8 text-center">
        <h2 className="text-sm font-semibold tracking-tight">
          Nothing loaded carries “{facet}”
        </h2>
        <p className="max-w-sm text-2xs leading-relaxed text-text-muted">
          The pivot is applied to the rows currently loaded, not to the whole index — a wider
          query may still contain this value.
        </p>
        <button
          type="button"
          onClick={onClearFacet}
          className="min-h-[var(--touch-target-min)] rounded-[var(--radius-chip)] border border-edge-subtle px-3 py-1 text-2xs text-text-secondary hover:border-accent hover:text-text-primary"
        >
          Clear the pivot
        </button>
      </div>
    );
  }
  if (failed) {
    return (
      <div className="flex h-full flex-col items-center justify-center gap-3 p-8 text-center">
        <h2 className="text-sm font-semibold tracking-tight">
          Some sources did not answer
        </h2>
        <p className="max-w-md text-2xs leading-relaxed text-text-muted">
          The sources that answered returned no visible rows, but at least one source is
          unavailable. A zero-result claim would be unsafe, so Explorer keeps this result
          partial.
        </p>
      </div>
    );
  }
  if (searching) {
    return (
      <div className="flex h-full flex-col items-center justify-center gap-3 p-8 text-center">
        <h2 className="text-sm font-semibold tracking-tight">
          {absence.confirmed ? `No source matched “${query}”` : `No rows loaded for “${query}”`}
        </h2>
        <p className="max-w-md text-2xs leading-relaxed text-text-muted">
          {absence.confirmed
            ? 'Every required source examined its full denominator with no unknown or omitted units, and the coordinator declared canonical finality.'
            : // The blocker in words, because "incomplete coverage" tells a
              // reader nothing they can act on, while "examined none of its 400
              // symbols" tells them to narrow the query.
              `${absence.reason}, so these bounded pages cannot establish global absence.`}
        </p>
        <EvidencePattern quality={absence.quality} />
        <button
          type="button"
          onClick={onClearQuery}
          className="min-h-[var(--touch-target-min)] rounded-[var(--radius-chip)] border border-edge-subtle px-3 py-1 text-2xs text-text-secondary hover:border-accent hover:text-text-primary"
        >
          Back to browsing
        </button>
      </div>
    );
  }
  return (
    <div className="flex h-full flex-col items-center justify-center gap-3 p-8 text-center">
      <h2 className="text-sm font-semibold tracking-tight">Nothing to browse yet</h2>
      <p className="max-w-md text-2xs leading-relaxed text-text-muted">
        The overview endpoints answered with no rows, so there is nothing indexed to show.
        Index a project, or search directly for a term.
      </p>
    </div>
  );
}
