import { Suspense, lazy, useState, type ComponentProps } from 'react';
import { ArrowLeft, Waypoints } from 'lucide-react';
import {
  DataRow,
  ExplorerSplit,
  InspectorPanel,
  KeyValueTree,
} from '../../ui/archetypes/ExplorerSplit.tsx';
import { CenteredState, LegacyBoundary } from '../../ui/ReadSection.tsx';
import { ActivityColumns } from '../../ui/ActivityColumns.tsx';
import { FigureRail, Readout } from '../../ui/instrument.tsx';
import { SearchField } from '../../ui/search/SearchField.tsx';
import { VirtualList } from '../../ui/VirtualList.tsx';
import { cn } from '../../ui/cn';
import { elideStart, splitCount } from '../../ui/format.ts';
import { ambiguityNote, annotateHubs, describeSubgraph, displayName } from './hubs.ts';
import { useLegacy } from '../../data/query/useLegacy.ts';
import type { LegacyResult } from '../../data/query/legacy.ts';
import { useCallback, useEffect, useMemo, useRef } from 'react';
import { GraphCanvas } from '../../viz/graph/GraphCanvas.tsx';
import { kindColorVars } from '../../viz/graph/kindColor.ts';
import { ActivationField } from '../../viz/graph/activation.ts';
import { IndexFreshness } from './IndexFreshness.tsx';
import { Strata } from './Strata.tsx';
import type { TraceFocus } from './TraceView.tsx';
import {
  type GraphNodeV1,
  type GraphOverviewPayloadV1,
  GraphOverviewPayloadV1Schema,
  type GraphSearchPayloadV1,
  GraphSearchPayloadV1Schema,
  type GraphSubgraphPayloadV1,
  GraphSubgraphPayloadV1Schema,
} from '../../contracts/generated.ts';

// Imports live at the top of a module; a `lazy` dynamic import is the
// documented exception, because the point is that the module is NOT fetched
// until it is needed. The trace drill-in is a thousand lines plus the whole of
// `viz/trace` — canvas renderer, spring integrator, palette — and most visits
// to this workspace never open it, so it is its own chunk rather than dead
// weight in the spine's. The `TraceFocus` import above stays a normal
// top-level type import: types are erased, so it costs nothing at runtime.
const TraceView = lazy(() =>
  import('./TraceView.tsx').then((m) => ({ default: m.TraceView })),
);

const BASE = '/api/plugins/graph';

/** Code: the connected graph itself (Sigma over the subgraph endpoint —
 * unseeded hub overview, reseeded on the selected symbol), kind composition,
 * symbol search, node inspector. The virtualized list beside the canvas is
 * its accessible equivalent. */
export function CodePage() {
  const overview = useLegacy(
    ['graph', 'overview'],
    `${BASE}/overview`,
    GraphOverviewPayloadV1Schema,
  );
  const [query, setQuery] = useState('');
  const [submitted, setSubmitted] = useState('');
  const search = useLegacy(
    ['graph', 'search', submitted],
    `${BASE}/search?q=${encodeURIComponent(submitted)}&limit=100`,
    GraphSearchPayloadV1Schema,
  );
  const [selected, setSelected] = useState<TraceFocus | null>(null);
  // The TRACE drill-in (plan 11b). It is a state of THIS page, not a route:
  // the spine and the trace are two zoom positions on one field, and a URL
  // change between them would break the "one navigable space" model. Escape
  // and the back control return to the spine.
  const [traced, setTraced] = useState<TraceFocus | null>(null);
  const subgraph = useLegacy(
    ['graph', 'subgraph', selected?.id ?? ''],
    `${BASE}/subgraph${selected ? `?node_id=${encodeURIComponent(selected.id)}` : ''}`,
    GraphSubgraphPayloadV1Schema,
  );
  const canvasNodes = useMemo(() => {
    if (subgraph.data?.outcome !== 'ok') return [];
    return subgraph.data.data.nodes.map((node) => ({
      id: node.id,
      label: node.name ?? node.qualified_name ?? node.id,
      kind: node.kind,
      degree: node.degree ?? undefined,
    }));
  }, [subgraph.data]);
  const canvasEdges = useMemo(() => {
    if (subgraph.data?.outcome !== 'ok') return [];
    return subgraph.data.data.edges.map((edge) => ({
      source: edge.source,
      target: edge.target,
      kind: edge.kind,
    }));
  }, [subgraph.data]);
  const activationRef = useRef(new ActivationField({ halfLifeMs: 3200 }));
  // Search results strike their nodes: querying the graph makes it fire.
  useEffect(() => {
    if (search.data?.outcome !== 'ok') return;
    const hits = (search.data.data.results ?? []).map((node) => node.id);
    if (hits.length) activationRef.current.strike(hits, 0.9);
  }, [search.data]);
  const selectFromCanvas = useCallback(
    (id: string | null) => {
      if (id == null) return setSelected(null);
      const node =
        subgraph.data?.outcome === 'ok'
          ? subgraph.data.data.nodes.find((candidate) => candidate.id === id)
          : undefined;
      if (node) setSelected(node);
    },
    [subgraph.data],
  );

  return (
    <ExplorerSplit
      filters={
        <div className="flex flex-col gap-3">
          <SearchField
            value={query}
            onChange={setQuery}
            onSubmit={() => setSubmitted(query.trim())}
            onClear={() => {
              setQuery('');
              setSubmitted('');
            }}
            label="Symbol search"
            placeholder="Search symbols"
            hint="press / to focus, Esc to clear"
            submitted={submitted}
          />
          <LegacyBoundary title="Graph" pending={overview.isPending} result={overview.data}>
            {(data) => {
              // Every total here is measured: `LegacyBoundary` runs this only
              // for `outcome: 'ok'`, and renders every other reading — including
              // the 500 this route raises on a failed query — as the failure it
              // was. So a zero here is a real zero and is printed as one.
              const kinds = (data.nodes_by_kind ?? [])
                .slice(0, 12)
                .map((k) => ({ label: k.kind, value: k.count, hint: 'nodes' }));
              return (
                <div className="flex flex-col gap-3">
                  {/* Node count is the headline of this whole product -- a
                   * graph of millions of symbols -- and it was set at 12px in
                   * a stack of three identical tiles. It takes the display
                   * tier; edges and files support it on one shared bezel. */}
                  <div className="flex flex-col">
                    <div className="td-raised border border-edge-subtle px-3 py-3">
                      <Readout
                        label="nodes"
                        size="xl"
                        value={splitCount(data.totals.nodes).value}
                        unit={splitCount(data.totals.nodes).unit}
                        note={`${data.totals.nodes.toLocaleString()} symbols indexed`}
                      />
                    </div>
                    <div className="flex border-x border-b border-edge-subtle bg-surface-1">
                      <div className="min-w-0 flex-1 px-3 py-2">
                        <Readout
                          label="edges"
                          size="sm"
                          value={splitCount(data.totals.edges).value}
                          unit={splitCount(data.totals.edges).unit}
                        />
                      </div>
                      <div className="min-w-0 flex-1 border-l border-edge-subtle px-3 py-2">
                        <Readout
                          label="files"
                          size="sm"
                          value={splitCount(data.totals.files).value}
                          unit={splitCount(data.totals.files).unit}
                        />
                      </div>
                    </div>
                  </div>
                  {kinds.length > 0 ? (
                    <figure className="flex flex-col gap-1.5">
                      <figcaption className="td-legend">composition by kind</figcaption>
                      <ActivityColumns buckets={kinds} height={56} />
                    </figure>
                  ) : null}
                </div>
              );
            }}
          </LegacyBoundary>
          {/* Connectedness is the spine's subject; layering is the other
           * structural reading of the same graph, and it belongs beside the
           * totals rather than inside the canvas — it is a property of the
           * whole index, not of the slice currently drawn. */}
          <Strata />
          {/* Both readings above are only as current as the generation they
           * were computed from, and that generation was sealed against one
           * exact source reference. This states which. */}
          <IndexFreshness />
        </div>
      }
      list={
        traced ? (
          <Suspense
            fallback={<TraceChunkFallback focus={traced} onClose={() => setTraced(null)} />}
          >
            <TraceView
              focus={traced}
              onClose={() => setTraced(null)}
              onFocusChange={(node) => {
                setTraced(node);
                setSelected(node);
              }}
            />
          </Suspense>
        ) : (
          // Two scroll containers, one inside the other: the archetype already
          // scrolls the list slot, and this pane pinned itself to `h-full` of
          // it and then divided that height between a canvas that cannot
          // shrink and a hub list that can. Below `md` the canvas alone is
          // taller than the pane, so the hub list resolved to `height: 0` and
          // took its scrollbar with it — "top 12 of 12,873" over nothing. It is
          // one scroll at narrow widths now: the pane grows to its content and
          // the archetype's scroller carries the whole column. The pinned
          // canvas with a separately scrolling list is kept from `md` up, where
          // there is room to divide, with a floor so the division can never
          // reach zero again.
          <div className="flex min-h-full flex-col md:h-full">
            <div className="flex flex-col gap-1.5 border-b border-edge-subtle p-3">
              <GraphSlicePane
                pending={subgraph.isPending}
                result={subgraph.data}
                nodes={canvasNodes}
                edges={canvasEdges}
                selectedId={selected?.id ?? null}
                onSelect={selectFromCanvas}
                activation={activationRef.current}
                totalNodes={
                  overview.data?.outcome === 'ok' ? overview.data.data.totals.nodes : null
                }
                seedLabel={selected ? displayName(selected) : null}
              />
            </div>
            <div className="md:min-h-[var(--pane-min-height)] md:flex-1 md:overflow-auto">
              {submitted === '' ? (
                <TopConnectedList
                  overviewPending={overview.isPending}
                  overviewResult={overview.data}
                  onSelect={(node) => {
                    // A hub card is the entry to TRACE: selecting the symbol and
                    // flooding its topography are one gesture, because "touch a
                    // symbol = TRACE floods" is the navigation model, not a
                    // secondary action hidden behind a second click.
                    setSelected(node);
                    setTraced(node);
                  }}
                  selected={selected}
                />
              ) : (
                <SymbolMatches
                  pending={search.isPending}
                  result={search.data}
                  submitted={submitted}
                  selected={selected}
                  onSelect={setSelected}
                />
              )}
            </div>
          </div>
        )
      }
      inspector={
        selected ? (
          <InspectorPanel title="Symbol" onClose={() => setSelected(null)}>
            <div className="flex flex-col gap-3">
              {/* The way into TRACE for anything reached by search, and the
               * keyboard path for everything else. The hub cards below open it
               * directly; this is the same drill-in for a symbol that was found
               * rather than ranked. */}
              <button
                type="button"
                onClick={() => setTraced(selected)}
                disabled={traced?.id === selected.id}
                className="flex items-center justify-center gap-1.5 rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-1 px-2 py-1 text-2xs text-text-secondary hover:bg-surface-2 hover:text-text-primary disabled:cursor-default disabled:text-text-muted"
              >
                <Waypoints aria-hidden size={12} />
                {traced?.id === selected.id ? 'Tracing this symbol' : 'Trace call topography'}
              </button>
              {selected.signature ? (
                <pre className="overflow-x-auto rounded-[var(--radius-standard)] bg-surface-2 p-2 font-mono text-2xs leading-relaxed">
                  {selected.signature}
                </pre>
              ) : null}
              {selected.file_path ? (
                <p className="font-mono text-2xs text-text-muted">
                  {selected.file_path}
                  {selected.start_line != null ? `:${selected.start_line}` : ''}
                </p>
              ) : null}
              <KeyValueTree value={selected} />
            </div>
          </InspectorPanel>
        ) : undefined
      }
    />
  );
}

/**
 * What stands in the list slot while the trace chunk is being fetched.
 *
 * The geometry is `TraceView`'s own opening row — same 32px header, same rule,
 * same padding, same back control in the same place — so the surface does not
 * shift when the chunk arrives and a keyboard reader is never stranded in a
 * pane with no way out of it.
 *
 * What it must NOT do is look like a trace. At this point no neighbour request
 * has been issued at all, so there is no field, no count and no symbol list to
 * stand in for; a skeleton of the readout plate or a `0` under `Callers` would
 * be a fabricated reading of a query that has not been asked. It reports the
 * one thing that is true — the module is still loading — and says plainly that
 * this is not an empty neighbourhood. The focus name is the symbol the reader
 * just chose, which is already in hand and not a guess.
 */
function TraceChunkFallback({
  focus,
  onClose,
}: {
  focus: TraceFocus;
  onClose: () => void;
}) {
  return (
    <div className="flex h-full min-h-0 flex-col" data-testid="trace-chunk-fallback">
      <header className="flex h-8 shrink-0 items-center gap-2.5 border-b border-edge-subtle px-2.5">
        <button
          type="button"
          onClick={onClose}
          className="flex shrink-0 items-center gap-1 text-2xs text-text-muted hover:text-text-primary focus-visible:text-text-primary"
        >
          <ArrowLeft aria-hidden size={12} />
          Back to spine
        </button>
        <span aria-hidden className="td-rule" />
        <h2 className="td-title min-w-0 truncate">
          <span className="text-text-muted">trace · </span>
          {displayName(focus)}
        </h2>
      </header>
      <div className="min-h-0 flex-1 overflow-auto">
        <p
          role="status"
          className="px-2.5 py-2 text-2xs leading-relaxed text-state-loading"
        >
          loading the trace view — the code for this surface is still arriving. No
          call edge has been requested yet, so nothing here is an empty
          neighbourhood, a zero or a settled field.
        </p>
      </div>
    </div>
  );
}

/** The graph slice above the list: the canvas, and the caption naming the rule
 * that picked what is on it. */
function GraphSlicePane({
  pending,
  result,
  nodes,
  edges,
  selectedId,
  onSelect,
  activation,
  totalNodes,
  seedLabel,
}: {
  pending: boolean;
  result: LegacyResult<GraphSubgraphPayloadV1> | undefined;
  nodes: ComponentProps<typeof GraphCanvas>['nodes'];
  edges: ComponentProps<typeof GraphCanvas>['edges'];
  selectedId: string | null;
  onSelect: (id: string | null) => void;
  activation: ComponentProps<typeof GraphCanvas>['activation'];
  totalNodes: number | null;
  seedLabel: string | null;
}) {
  return (
    <LegacyBoundary title="Code graph" pending={pending} result={result}>
      {(payload) => {
        if (payload.nodes.length === 0) {
          // An answered read that returned no symbols, said as the measurement
          // it is. The slice route reports its own failures with a non-2xx the
          // boundary renders instead of this, so reaching here means the graph
          // holds nothing.
          return (
            <CenteredState
              title="No symbols are indexed for this project"
              kind="complete_zero_findings"
            />
          );
        }
        return (
          <>
            <GraphCanvas
              nodes={nodes}
              edges={edges}
              selectedId={selectedId}
              onSelect={onSelect}
              height={300}
              canvasClassName="md:min-h-[400px]"
              activation={activation}
              fallbackDescription="the symbol results and inspector below remain available as a text alternative"
              encoding={{
                body: 'symbol',
                size: 'connectedness',
                hue: 'symbol kind',
                signal: 'search or click activation',
                relation: 'relation; activation thickens',
              }}
            />
            {/* Eighty nodes out of a hundred and eighteen thousand is not a
              * reading until the rule that picked them is stated: "the busiest
              * connected region" and "one symbol's neighbours" are different
              * claims about identically-shaped pictures. Both branches are read
              * off the endpoint's own `mode`. */}
            <SubgraphCaption payload={payload} totalNodes={totalNodes} seedLabel={seedLabel} />
          </>
        );
      }}
    </LegacyBoundary>
  );
}

/** The symbol search results, as the accessible equivalent of the canvas above
 * them. The header states whether the rows are the whole match set, because the
 * route caps them and a capped page presented as a total would understate the
 * graph. */
function SymbolMatches({
  pending,
  result,
  submitted,
  selected,
  onSelect,
}: {
  pending: boolean;
  result: LegacyResult<GraphSearchPayloadV1> | undefined;
  submitted: string;
  selected: TraceFocus | null;
  onSelect: (node: GraphNodeV1) => void;
}) {
  return (
    <LegacyBoundary title="Symbols" pending={pending} result={result}>
      {(data) => {
        const rows = data.results ?? [];
        if (rows.length === 0)
          return (
            <CenteredState
              title={`No symbol matches ${submitted}`}
              kind="complete_zero_findings"
            />
          );
        const capped = data.total != null && data.total > rows.length;
        const degreeCeiling = rows.reduce((max, node) => Math.max(max, node.degree ?? 0), 0);
        return (
          <VirtualList
            items={rows}
            getKey={(node) => node.id}
            header={
              <p className="td-legend border-b border-edge-subtle px-3 py-2">
                {capped
                  ? `${rows.length} of ${data.total} matches`
                  : `${data.total ?? rows.length} matches`}
              </p>
            }
            renderItem={(node) => (
              <SymbolRow
                node={node}
                degreeCeiling={degreeCeiling}
                selected={selected?.id === node.id}
                onSelect={() => onSelect(node)}
              />
            )}
          />
        );
      }}
    </LegacyBoundary>
  );
}

/** Default view when no search is active: the graph's most connected symbols,
 * drawn as two joined instruments.
 *
 *   the SPINE   every hub as a mark on one shared degree axis, so position is
 *               the measurement and the shape of the drop is directly visible.
 *               Hue is the symbol's kind, from the same `kindColor` rule the
 *               canvas above paints with.
 *
 *   the FIELD   the same hubs as cards on a dense grid, where the NAME's type
 *               size falls with rank — magnitude read as typography, costing
 *               no horizontal column.
 *
 * The endpoint (`graph_queries::top_connected_rows`) serves the top twelve rows
 * and five fields. So the spine is captioned as those hubs only, its axis is
 * anchored at zero and labelled with the real extremes, and nothing here claims
 * to be the whole graph's degree distribution — which this payload does not
 * contain. Search reaches everything else. */
function TopConnectedList({
  overviewPending,
  overviewResult,
  onSelect,
  selected,
}: {
  overviewPending: boolean;
  overviewResult: LegacyResult<GraphOverviewPayloadV1> | undefined;
  onSelect: (node: GraphNodeV1) => void;
  selected: TraceFocus | null;
}) {
  return (
    <LegacyBoundary title="Code" pending={overviewPending} result={overviewResult}>
      {(payload) => {
        const hubs = payload.top_connected;
        if (hubs.length === 0)
          return (
            <CenteredState
              title="No connected symbols are indexed for this project"
              kind="complete_zero_findings"
            />
          );
        return (
          <HubField
            hubs={hubs}
            indexedNodes={payload.totals.nodes}
            onSelect={onSelect}
            selected={selected}
          />
        );
      }}
    </LegacyBoundary>
  );
}

/** The canvas's selection rule, printed under it. See `hubs.ts` for where each
 * sentence comes from in `subgraph_payload`. */
function SubgraphCaption({
  payload,
  totalNodes,
  seedLabel,
}: {
  payload: GraphSubgraphPayloadV1 | undefined;
  totalNodes: number | null;
  seedLabel: string | null;
}) {
  const caption = describeSubgraph(payload, totalNodes, seedLabel);
  if (!caption) return null;
  return (
    <figcaption className="flex flex-col gap-0.5">
      <span className="flex items-baseline gap-2">
        <span className="td-legend shrink-0">{caption.scale}</span>
        <span aria-hidden className="td-rule" />
        {caption.capped ? (
          <span className="td-legend shrink-0 text-text-muted">capped at the limit</span>
        ) : null}
      </span>
      <span className="text-3xs leading-relaxed text-text-muted">{caption.rule}</span>
    </figcaption>
  );
}

/** How a hub's degree is drawn on the spine, and how big its name is set in
 * the field below. Both derive from the same rank/degree pair, so the two
 * instruments cannot disagree about which symbol matters most. */
function markDiameter(degree: number, ceiling: number): number {
  // Area, not diameter, tracks the value: a disc twice as wide reads as four
  // times as much, so degree goes through a square root before it becomes a
  // width. Floor of 6px keeps the twelfth hub a body rather than a speck.
  return 6 + 12 * Math.sqrt(ceiling > 0 ? Math.max(0, degree) / ceiling : 0);
}

/** Type tier by rank. Discrete steps rather than a continuous scale: four
 * legible sizes read as a hierarchy, whereas twelve near-identical ones read
 * as sloppy typesetting. */
function nameTier(rank: number): string {
  if (rank === 0) return 'td-display text-xl';
  if (rank < 3) return 'td-value text-base font-medium';
  if (rank < 6) return 'td-value text-sm';
  return 'td-value text-xs';
}

function HubField({
  hubs,
  indexedNodes,
  onSelect,
  selected,
}: {
  hubs: GraphNodeV1[];
  indexedNodes: number | null;
  onSelect: (node: GraphNodeV1) => void;
  selected: TraceFocus | null;
}) {
  // The endpoint already orders by degree, but the view's whole grammar is
  // rank, so it establishes rank itself rather than trusting arrival order.
  const ranked = useMemo(
    () =>
      hubs
        .filter((node) => typeof node.degree === 'number')
        .sort((a, b) => (b.degree ?? 0) - (a.degree ?? 0)),
    [hubs],
  );
  if (ranked.length === 0)
    return (
      <p className="p-6 text-center text-sm text-text-muted">
        the graph reported hubs without connection counts, so there is nothing
        to rank
      </p>
    );
  const ceiling = ranked[0]?.degree ?? 0;
  const floor = ranked[ranked.length - 1]?.degree ?? 0;
  const leadName = displayName(ranked[0]);
  const tailName = displayName(ranked[ranked.length - 1]);
  // Eight of the twelve hubs on a real Rust graph are language primitives or
  // one-word generics — `path`, `json`, `u64`, `Value`, `trim`, `kind` — and
  // two of them are literally the same word. `qualified_name` is not served on
  // this route, so the file is the only thing that can tell them apart.
  const annotated = annotateHubs(ranked);
  const ambiguity = ambiguityNote(annotated);

  return (
    <div className="flex flex-col">
      <div className="flex items-center gap-2.5 border-b border-edge-subtle px-3 py-2">
        <span className="td-legend">most connected symbols</span>
        <span aria-hidden className="td-rule" />
        {/* The provenance of the twelve. Dropped below `sm`, where the legend
         * to its left already fills the strip and this would clip mid-number
         * — a half-printed total is worse than no total. */}
        <span className="td-legend shrink-0 normal-case tracking-normal max-sm:hidden">
          {indexedNodes != null
            ? `top ${ranked.length} of ${indexedNodes.toLocaleString()} by degree`
            : `top ${ranked.length} by degree`}
        </span>
      </div>

      {/* ---- the spine ---------------------------------------------------
       * One axis, one mark per hub, position = connection count. The marks
       * are not controls: reading them is the point, and the ranked cards
       * below are their exact accessible and keyboard equivalent (the same
       * pattern the graph canvas uses). Overlap is real information here —
       * hubs whose degrees nearly tie genuinely land on top of one another,
       * and a hairline of substrate around each body keeps that legible as a
       * cluster instead of a blob. */}
      <figure className="flex flex-col gap-1.5 border-b border-edge-subtle px-3 pb-1.5 pt-2">
        <div
          className="flex items-center gap-2"
          role="img"
          aria-label={`Connectivity spine: ${ranked.length} most connected symbols plotted by connection count on one axis from 0 to ${ceiling.toLocaleString()}. ${leadName} leads with ${ceiling.toLocaleString()} connections; the lowest ranked, ${tailName}, has ${floor.toLocaleString()}. The ranked cards below carry the same symbols.`}
        >
          <span className="td-value shrink-0 text-3xs text-text-muted" data-cell="numeric">
            0
          </span>
          <span className="relative mx-1.5 h-6 min-w-0 flex-1">
            <span
              aria-hidden
              className="absolute inset-x-0 top-1/2 h-px -translate-y-1/2 bg-edge-strong"
            />
            {/* Graduations, so a mark's position is read against a scale
             * rather than estimated against a bare line. */}
            <span
              aria-hidden
              className="absolute inset-x-0 top-1/2 h-1.5 opacity-70"
              style={{
                backgroundImage:
                  'repeating-linear-gradient(to right, var(--raw-edge-strong) 0 1px, transparent 1px 10%)',
              }}
            />
            {ranked.map((node, rank) => {
              const degree = node.degree ?? 0;
              const size = markDiameter(degree, ceiling);
              return (
                <span
                  key={node.id ?? rank}
                  aria-hidden
                  title={`${displayName(node)} · ${node.kind} · ${degree.toLocaleString()} deg — click to trace`}
                  // A pointer shortcut into the same drill-in the card opens.
                  // The mark stays a mark: not focusable, not exposed to AT,
                  // no accessible name of its own — because the ranked card
                  // below IS its keyboard and screen-reader equivalent, and
                  // duplicating it as a control would make a reader walk the
                  // same twelve symbols twice.
                  onClick={() => onSelect(node)}
                  className="absolute top-1/2 cursor-pointer rounded-full bg-[var(--kind-dark)] [[data-theme=light]_&]:bg-[var(--kind-light)]"
                  style={{
                    ...kindColorVars(node.kind),
                    left: `${ceiling > 0 ? (degree / ceiling) * 100 : 0}%`,
                    width: size,
                    height: size,
                    transform: 'translate(-50%, -50%)',
                    // Lower-degree marks draw on top, so a body never hides
                    // inside a larger one it happens to have tied with.
                    zIndex: rank + 1,
                    boxShadow: '0 0 0 1.5px var(--raw-surface-0)',
                  }}
                />
              );
            })}
          </span>
          <span
            className="td-value shrink-0 text-3xs text-text-secondary"
            data-cell="numeric"
          >
            {ceiling.toLocaleString()}
            <span className="td-unit ml-1">deg</span>
          </span>
        </div>
        {/* The one direct label the spine carries, and it shares the caption's
         * line rather than owning a row of its own: flush under the far-right
         * mark, so it names that body by pure proximity. A label over the low
         * end was tried and dropped — to clear its neighbours it has to sit at
         * the axis origin, and a mark only a fifth of the way along an axis
         * anchored at zero is exactly the one that must not be mislabelled. */}
        <figcaption className="flex items-baseline gap-3 leading-tight">
          <span className="min-w-0 flex-1 truncate text-3xs text-text-muted">
            position = connections · size = connections · hue = kind, on the
            same scale as the field above · axis anchored at zero
          </span>
          <span className="td-value shrink-0 text-2xs text-text-secondary">
            {leadName}
          </span>
        </figcaption>
      </figure>

      {/* ---- the field ---------------------------------------------------
       * Hairline-ruled cells sharing one grid. The rules are cell borders, not
       * a 1px gap over an edge-coloured backing: a backing shows through the
       * implicit cells of a part-filled last row, which paints a solid block
       * of rule colour where the grid simply has no data — a fabricated
       * region, and the one thing this console must never draw. */}
      {ambiguity ? (
        <p className="border-b border-edge-subtle px-3 py-1.5 text-3xs leading-relaxed text-text-muted">
          {ambiguity}
        </p>
      ) : null}

      <ol className="grid grid-cols-1 sm:grid-cols-2 md:grid-cols-3 xl:grid-cols-4">
        {annotated.map(({ hub: node, display, module, file, ambiguous }, rank) => (
          <li
            key={node.id ?? rank}
            className={cn(
              'min-w-0 border-b border-l border-edge-subtle',
              // The leader's name is set at the display tier, and a display
              // numeral in a 200px cell is a name that truncates — which
              // defeats the entire point of sizing it. It takes two columns
              // wherever a column is narrow, and drops back to one at `xl`,
              // where a quarter of the workspace is already wide enough.
              rank === 0 && 'sm:col-span-2 xl:col-span-1',
            )}
          >
            <HubCard
              node={node}
              display={display}
              rank={rank}
              module={module}
              file={file}
              ambiguous={ambiguous}
              selected={selected?.id === node.id}
              onSelect={() => onSelect(node)}
            />
          </li>
        ))}
      </ol>
    </div>
  );
}

function HubCard({
  node,
  display,
  rank,
  module,
  file,
  ambiguous,
  selected,
  onSelect,
}: {
  node: GraphNodeV1;
  /** The headline, already resolved by `annotateHubs`. */
  display: string;
  rank: number;
  /** Directory the symbol lives in, trailing slash included. */
  module: string;
  /** File name alone — the part that actually disambiguates. */
  file: string;
  /** Another card in this set carries the same name. */
  ambiguous: boolean;
  selected: boolean;
  onSelect: () => void;
}) {
  const degree = node.degree ?? 0;
  return (
    <button
      type="button"
      onClick={onSelect}
      aria-pressed={selected}
      className={cn(
        'relative flex h-full min-h-[var(--touch-target-min)] w-full flex-col gap-0.5 px-3 py-1.5 text-left',
        selected ? 'bg-surface-2' : 'bg-surface-0 hover:bg-surface-1',
        'focus-visible:bg-surface-1',
      )}
    >
      <span
        aria-hidden
        className={cn(
          'absolute inset-y-0 left-0 w-[2px]',
          selected ? 'bg-accent' : 'bg-transparent',
        )}
      />
      {/* `leading-tight` on both lines, not the inherited body 1.5: twelve
       * cards multiply half a line of leading into the better part of a row,
       * and this grid is here to give the workspace its vertical budget back. */}
      <span className="flex min-w-0 items-baseline gap-2 leading-tight">
        <span className="td-legend shrink-0" data-cell="numeric">
          {String(rank + 1).padStart(2, '0')}
        </span>
        {/* The kind's hue, repeated from this symbol's mark on the spine so a
         * card can be found in the distribution and back again. Colour never
         * carries it alone — the kind is spelled out on the line below. */}
        <span
          aria-hidden
          className="size-1.5 shrink-0 translate-y-[-1px] rounded-full bg-[var(--kind-dark)] [[data-theme=light]_&]:bg-[var(--kind-light)]"
          style={kindColorVars(node.kind)}
        />
        <span
          className={cn('min-w-0 flex-1 truncate text-text-primary', nameTier(rank))}
          title={node.qualified_name ?? undefined}
        >
          {display}
        </span>
        {/* The leader's own count steps up with its name: the one symbol the
         * eye lands on first should not have to report itself in the same
         * whisper as the twelfth. */}
        <span
          className={cn(
            'td-value shrink-0 self-baseline text-text-secondary',
            rank === 0 ? 'text-sm' : 'text-2xs',
          )}
          data-cell="numeric"
        >
          {degree.toLocaleString()}
          <span className="td-unit ml-1">deg</span>
        </span>
      </span>
      {/* The context line. The FILE leads it at the same size as the kind
        * beside it, with the directory trailing quietly: between two cards both
        * called `path`, "graph_api.rs" is the answer. When the name is
        * genuinely ambiguous inside this set the file steps up to the primary
        * text colour — it is then not context, it is the identifier. */}
      <span className="flex min-w-0 items-baseline gap-2 pl-6 leading-tight">
        <span className="td-legend max-w-20 shrink-0 truncate">{node.kind}</span>
        {/* The file is capped at three fifths of the line and truncates inside
          * that cap rather than running past the card's own border, which is
          * what an unshrinkable span did at 768px. */}
        <span
          className={cn(
            'td-value max-w-[60%] shrink-0 truncate text-2xs',
            ambiguous ? 'text-text-primary' : 'text-text-secondary',
          )}
          title={node.file_path ?? undefined}
        >
          {file || '—'}
        </span>
        <span
          className="td-value min-w-0 flex-1 truncate text-right text-3xs text-text-muted max-2xl:hidden"
          title={node.file_path ?? undefined}
        >
          {elideStart(module.replace(/\/$/, ''), 24)}
        </span>
      </span>
    </button>
  );
}

function SymbolRow({
  node,
  degreeCeiling,
  selected,
  onSelect,
}: {
  node: GraphNodeV1;
  degreeCeiling: number;
  selected: boolean;
  onSelect: () => void;
}) {
  return (
    <DataRow selected={selected} onSelect={onSelect}>
      {/* Under 768px the symbol's own name and its connectedness are the row;
       * kind and file path are what a narrow viewport gives up. Carrying all
       * four columns left the name -- the thing being listed -- with no width
       * at all. */}
      <span className="td-legend w-20 shrink-0 truncate max-md:hidden">{node.kind}</span>
      {/* Qualified name first, deliberately the reverse of `displayName`: the
       * search route does serve it, and in a list of matches the module path
       * is what separates two hits that share a bare name. */}
      <span className="td-value min-w-0 flex-1 truncate text-text-primary">
        {node.qualified_name ?? node.name ?? node.id}
      </span>
      {node.degree != null ? (
        <FigureRail
          value={node.degree}
          unit="deg"
          fraction={degreeCeiling > 0 ? node.degree / degreeCeiling : null}
        />
      ) : null}
      <span
        className="td-value w-52 shrink-0 truncate text-right text-2xs text-text-muted max-md:hidden"
        title={node.file_path ?? undefined}
      >
        {elideStart(node.file_path, 29)}
      </span>
    </DataRow>
  );
}
