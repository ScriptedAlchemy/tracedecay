import { useMemo, useState } from 'react';
import { Boxes, MessagesSquare, Lightbulb, type LucideIcon } from 'lucide-react';
import { z } from 'zod';
import {
  DataRow,
  ExplorerSplit,
  InspectorPanel,
  ListCaption,
  RawFields,
  RESULT_ROW_HEIGHT,
} from '../../ui/archetypes/ExplorerSplit.tsx';
import { StateChip } from '../../ui/StateChip';
import { VirtualList } from '../../ui/VirtualList.tsx';
import { Highlight, MetaLabel } from '../../ui/search/Highlight.tsx';
import { FacetGroup } from '../../ui/search/Facets.tsx';
import { SearchField } from '../../ui/search/SearchField.tsx';
import { queryTerms } from '../../ui/search/terms.ts';
import { cn } from '../../ui/cn';
import { Meter } from '../../ui/instrument.tsx';
import { AnyObject, type LegacyResult } from '../../data/query/legacy.ts';
import { useLegacy } from '../../data/query/useLegacy.ts';
import {
  LANES,
  codeHits,
  facetCounts,
  knowledgeHits,
  relativeTime,
  sessionHits,
  type Hit,
  type LaneId,
} from './model.ts';

/* ---------------------------------------------------------------- payloads */

const GraphSearchPayload = z
  .object({ results: z.array(AnyObject).optional(), total: z.number().optional() })
  .passthrough();
const GraphOverviewPayload = z
  .object({ top_connected: z.array(AnyObject).optional() })
  .passthrough();
const LcmSearchPayload = z
  .object({
    matches: z
      .object({
        messages: z.array(AnyObject).optional(),
        summary_nodes: z.array(AnyObject).optional(),
      })
      .passthrough()
      .optional(),
    total: z
      .object({
        messages: z.number().optional(),
        summary_nodes: z.number().optional(),
      })
      .passthrough()
      .optional(),
  })
  .passthrough();
const LcmOverviewPayload = z
  .object({
    latest_summary_nodes: z.array(AnyObject).optional(),
    overview: z.object({ messages_total: z.number().optional() }).passthrough().optional(),
  })
  .passthrough();
const MemoryPayload = z
  .object({
    holographic: z
      .object({
        facts: z.array(AnyObject).optional(),
        overview: z.object({ facts: z.number().optional() }).passthrough().optional(),
      })
      .passthrough()
      .optional(),
  })
  .passthrough();

const LANE_ICON: Record<LaneId, LucideIcon> = {
  code: Boxes,
  sessions: MessagesSquare,
  knowledge: Lightbulb,
};

interface Lane {
  readonly id: LaneId;
  readonly hits: Hit[];
  readonly pending: boolean;
  readonly outcome: LegacyResult<unknown>['outcome'] | 'pending' | 'unknown';
  /** Size of the matching set the daemon reports, when it reports one. */
  readonly reportedTotal?: number | undefined;
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
  const [laneFilter, setLaneFilter] = useState<LaneId | null>(null);
  const [facet, setFacet] = useState<{ lane: LaneId; value: string } | null>(null);
  const [selected, setSelected] = useState<Hit | null>(null);
  const searching = submitted !== '';
  const terms = useMemo(() => queryTerms(submitted), [submitted]);

  const encoded = encodeURIComponent(submitted);
  const graphSearch = useLegacy(
    ['explorer', 'graph', submitted],
    `/api/plugins/graph/search?q=${encoded}`,
    GraphSearchPayload,
    { enabled: searching },
  );
  const graphBrowse = useLegacy(
    ['explorer', 'graph-overview'],
    '/api/plugins/graph/overview',
    GraphOverviewPayload,
    { enabled: !searching },
  );
  const lcmSearch = useLegacy(
    ['explorer', 'lcm', submitted],
    `/api/plugins/hermes-lcm/search?q=${encoded}&limit=50`,
    LcmSearchPayload,
    { enabled: searching },
  );
  const lcmBrowse = useLegacy(
    ['explorer', 'lcm-overview'],
    '/api/plugins/hermes-lcm/overview',
    LcmOverviewPayload,
    { enabled: !searching },
  );
  const memory = useLegacy(
    ['explorer', 'memory', submitted],
    `/api/plugins/holographic/?q=${encoded}&limit=25`,
    MemoryPayload,
  );

  const lanes: Lane[] = useMemo(() => {
    const code = searching ? graphSearch : graphBrowse;
    const sessions = searching ? lcmSearch : lcmBrowse;
    const codeRows =
      code.data?.outcome === 'ok'
        ? searching
          ? ((code.data.data as z.infer<typeof GraphSearchPayload>).results ?? [])
          : ((code.data.data as z.infer<typeof GraphOverviewPayload>).top_connected ?? [])
        : [];
    const sessionRows =
      sessions.data?.outcome === 'ok'
        ? searching
          ? [
              ...((sessions.data.data as z.infer<typeof LcmSearchPayload>).matches?.messages ??
                []),
              ...((sessions.data.data as z.infer<typeof LcmSearchPayload>).matches
                ?.summary_nodes ?? []),
            ]
          : ((sessions.data.data as z.infer<typeof LcmOverviewPayload>).latest_summary_nodes ??
            [])
        : [];
    const factRows =
      memory.data?.outcome === 'ok'
        ? ((memory.data.data as z.infer<typeof MemoryPayload>).holographic?.facts ?? [])
        : [];
    const codeTotal =
      searching && code.data?.outcome === 'ok'
        ? (code.data.data as z.infer<typeof GraphSearchPayload>).total
        : undefined;
    const sessionTotal =
      searching && sessions.data?.outcome === 'ok'
        ? ((sessions.data.data as z.infer<typeof LcmSearchPayload>).total?.messages ?? 0) +
          ((sessions.data.data as z.infer<typeof LcmSearchPayload>).total?.summary_nodes ?? 0)
        : undefined;
    return [
      {
        id: 'code' as const,
        hits: codeHits(codeRows, terms),
        pending: code.isPending,
        outcome: code.isPending ? ('pending' as const) : (code.data?.outcome ?? 'unknown'),
        ...(codeTotal != null ? { reportedTotal: codeTotal } : {}),
      },
      {
        id: 'sessions' as const,
        hits: sessionHits(sessionRows, terms),
        pending: sessions.isPending,
        outcome: sessions.isPending
          ? ('pending' as const)
          : (sessions.data?.outcome ?? 'unknown'),
        ...(sessionTotal != null ? { reportedTotal: sessionTotal } : {}),
      },
      {
        id: 'knowledge' as const,
        hits: knowledgeHits(factRows, terms),
        pending: memory.isPending,
        outcome: memory.isPending ? ('pending' as const) : (memory.data?.outcome ?? 'unknown'),
      },
    ];
  }, [searching, graphSearch, graphBrowse, lcmSearch, lcmBrowse, memory, terms]);

  const laneById = useMemo(
    () => new Map(lanes.map((lane) => [lane.id, lane])),
    [lanes],
  );
  const visibleLanes = laneFilter ? lanes.filter((l) => l.id === laneFilter) : lanes;
  const laneHits = visibleLanes.flatMap((lane) => lane.hits);
  const hits = facet
    ? laneHits.filter((hit) => hit.lane === facet.lane && hit.facet === facet.value)
    : laneHits;
  const anyPending = lanes.some((lane) => lane.pending);
  const failedLanes = lanes.filter(
    (lane) => lane.outcome !== 'ok' && lane.outcome !== 'pending',
  );

  const reset = () => {
    setQuery('');
    setSubmitted('');
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
              one query, fanned across the three memories the daemon keeps
            </p>
          </div>
          <div className="flex min-w-0 flex-col gap-3 lg:flex-row lg:items-start">
            <SearchField
              value={query}
              onChange={setQuery}
              onSubmit={() => {
                setSubmitted(query.trim());
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
                    daemon&rsquo;s own order · terms are marked where they occur in the payload
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
              {LANES.map((spec) => {
                const lane = laneById.get(spec.id);
                const Icon = LANE_ICON[spec.id];
                const active = laneFilter === spec.id;
                return (
                  <button
                    key={spec.id}
                    type="button"
                    aria-pressed={active}
                    onClick={() => {
                      setLaneFilter(active ? null : spec.id);
                      setFacet(null);
                    }}
                    className={cn(
                      'flex min-w-[7.5rem] flex-col gap-1 rounded-[var(--radius-standard)] border px-2.5 py-1.5 text-left',
                      active
                        ? 'border-accent bg-surface-2'
                        : 'border-edge-subtle bg-surface-0 hover:border-edge-strong',
                    )}
                  >
                    <span className="flex items-center gap-1.5">
                      <Icon aria-hidden size={12} className={spec.textClass} />
                      <span className="text-2xs font-medium text-text-secondary">
                        {spec.label}
                      </span>
                    </span>
                    <span className="flex items-baseline gap-1.5">
                      {lane?.pending ? (
                        <StateChip kind="loading" />
                      ) : lane?.outcome === 'ok' ? (
                        <>
                          <span className="tabular text-sm font-semibold leading-none text-text-primary">
                            {lane.hits.length.toLocaleString()}
                          </span>
                          <span className="text-2xs text-text-muted">
                            {lane.reportedTotal != null
                              ? `of ${lane.reportedTotal.toLocaleString()}`
                              : searching
                                ? 'hits'
                                : 'shown'}
                          </span>
                        </>
                      ) : (
                        <StateChip
                          kind={lane?.outcome === 'offline' ? 'offline' : 'error'}
                        />
                      )}
                    </span>
                  </button>
                );
              })}
            </div>
          </div>
        </div>
      }
      filters={
        <div className="flex flex-col gap-4">
          {visibleLanes.map((lane) => {
            const spec = LANES.find((l) => l.id === lane.id)!;
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
          <section className="flex flex-col gap-2">
            <MetaLabel>What each lane searches</MetaLabel>
            <dl className="flex flex-col gap-2">
              {LANES.map((spec) => {
                const lane = laneById.get(spec.id);
                return (
                  <div key={spec.id} className="flex gap-2">
                    <span
                      aria-hidden
                      className={cn('mt-1 h-3 w-[3px] shrink-0 rounded-full', spec.railClass)}
                    />
                    <span className="min-w-0">
                      <dt className="text-2xs font-medium text-text-secondary">
                        {spec.label}
                      </dt>
                      <dd className="text-2xs leading-relaxed text-text-muted">
                        {searching ? spec.searches : spec.browseLabel}
                        {lane?.reportedTotal != null
                          ? ` · daemon reports ${lane.reportedTotal.toLocaleString()} matching`
                          : ''}
                      </dd>
                    </span>
                  </div>
                );
              })}
            </dl>
          </section>
          {failedLanes.length > 0 ? (
            <section className="flex flex-col gap-1.5">
              <MetaLabel>Unanswered</MetaLabel>
              {failedLanes.map((lane) => (
                <p key={lane.id} className="flex items-center gap-1.5 text-2xs text-text-muted">
                  <StateChip
                    kind={lane.outcome === 'offline' ? 'offline' : 'error'}
                  />
                  <span>{LANES.find((l) => l.id === lane.id)?.label}</span>
                </p>
              ))}
              <p className="text-2xs leading-relaxed text-text-muted">
                Results below are only from the lanes that answered. Nothing is being
                substituted for the rest.
              </p>
            </section>
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
                <span className="tabular font-medium text-text-secondary">
                  {hits.length.toLocaleString()}
                </span>
                <span>
                  {searching ? 'results' : 'rows'}
                  {laneFilter
                    ? ` in ${LANES.find((l) => l.id === laneFilter)?.label.toLowerCase()}`
                    : ' across three memories'}
                  {facet ? ` · ${facet.value}` : ''}
                </span>
                <span aria-hidden className="ml-auto hidden sm:inline">
                  ordered by each memory&rsquo;s own ranking
                </span>
              </ListCaption>
            }
            renderItem={(hit) => (
              <HitRow
                hit={hit}
                terms={terms}
                selected={selected?.key === hit.key}
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

/* ------------------------------------------------------------------- rows */

function HitRow({
  hit,
  terms,
  selected,
  onSelect,
}: {
  hit: Hit;
  terms: readonly string[];
  selected: boolean;
  onSelect: () => void;
}) {
  const spec = LANES.find((lane) => lane.id === hit.lane)!;
  const Icon = LANE_ICON[hit.lane];
  const age = relativeTime(hit.stamp);
  return (
    <DataRow
      selected={selected}
      onSelect={onSelect}
      height={RESULT_ROW_HEIGHT}
      railClassName={spec.railClass}
      className="pl-4"
    >
      <span className="flex w-10 shrink-0 flex-col items-center gap-0.5">
        <Icon aria-hidden size={13} className={cn(spec.textClass, 'opacity-80')} />
        <span className="tabular text-2xs leading-none text-text-muted">#{hit.rank}</span>
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
              fraction={hit.signal.max <= 0 ? 0 : hit.signal.value / hit.signal.max}
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
  const spec = LANES.find((lane) => lane.id === hit.lane)!;
  const Icon = LANE_ICON[hit.lane];
  const age = relativeTime(hit.stamp);
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
                Position {hit.rank} in this memory&rsquo;s answer. The query text occurs in{' '}
                <span className="font-mono text-text-primary">
                  {hit.matchedIn.join(', ')}
                </span>
                .
              </>
            ) : (
              <>
                Position {hit.rank} in this memory&rsquo;s answer. The daemon matched on its
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
                fraction={hit.signal.max <= 0 ? 0 : hit.signal.value / hit.signal.max}
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
        <RawFields value={hit.raw} />
      </div>
    </InspectorPanel>
  );
}

/* ----------------------------------------------------------- empty states */

function EmptyResults({
  searching,
  pending,
  query,
  facet,
  failed,
  onClearFacet,
  onClearQuery,
}: {
  searching: boolean;
  pending: boolean;
  query: string;
  facet: string | null;
  failed: boolean;
  onClearFacet: () => void;
  onClearQuery: () => void;
}) {
  if (pending) {
    return (
      <div className="flex h-full flex-col items-center justify-center gap-3 p-8 text-center">
        <StateChip kind="loading" />
        <p className="text-2xs text-text-muted">Reading from the daemon.</p>
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
          className="rounded-[var(--radius-chip)] border border-edge-subtle px-2 py-1 text-2xs text-text-secondary hover:border-accent hover:text-text-primary"
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
          Some memories did not answer
        </h2>
        <p className="max-w-md text-2xs leading-relaxed text-text-muted">
          The lanes that answered returned no visible rows, but at least one lane is
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
          No memory answered for “{query}”
        </h2>
        <p className="max-w-md text-2xs leading-relaxed text-text-muted">
          All three lanes returned successfully and all three returned nothing. The term is
          genuinely absent from the indexed symbols, the stored transcripts, and the fact
          store — it is not being filtered out here.
        </p>
        <button
          type="button"
          onClick={onClearQuery}
          className="rounded-[var(--radius-chip)] border border-edge-subtle px-2 py-1 text-2xs text-text-secondary hover:border-accent hover:text-text-primary"
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
