import { useState } from 'react';
import { Search } from 'lucide-react';
import {
  DataRow,
  ExplorerSplit,
  InspectorPanel,
  KeyValueTree,
} from '../../ui/archetypes/ExplorerSplit.tsx';
import { LegacyBoundary } from '../../ui/LegacyStates.tsx';
import { ActivityColumns } from '../../ui/ActivityColumns.tsx';
import { Meter, Readout } from '../../ui/instrument.tsx';
import { VirtualList } from '../../ui/VirtualList.tsx';
import { cn } from '../../ui/cn';
import { elideStart, splitCount } from '../../ui/format.ts';
import { useLegacy } from '../../data/query/useLegacy.ts';
import { useCallback, useEffect, useMemo, useRef } from 'react';
import { GraphCanvas } from '../../viz/graph/GraphCanvas.tsx';
import { kindColorVars } from '../../viz/graph/kindColor.ts';
import { ActivationField } from '../../viz/graph/activation.ts';
import {
  GraphOverviewPayloadSchema,
  GraphSearchPayloadSchema,
  SubgraphPayloadSchema,
  type GraphNode,
} from './contracts.ts';

const BASE = '/api/plugins/graph';

/** Code: the connected graph itself (Sigma over the subgraph endpoint —
 * unseeded hub overview, reseeded on the selected symbol), kind composition,
 * symbol search, node inspector. The virtualized list beside the canvas is
 * its accessible equivalent. */
export function CodePage() {
  const overview = useLegacy(
    ['graph', 'overview'],
    `${BASE}/overview`,
    GraphOverviewPayloadSchema,
  );
  const [query, setQuery] = useState('');
  const [submitted, setSubmitted] = useState('');
  const search = useLegacy(
    ['graph', 'search', submitted],
    `${BASE}/search?q=${encodeURIComponent(submitted)}&limit=100`,
    GraphSearchPayloadSchema,
  );
  const [selected, setSelected] = useState<GraphNode | null>(null);
  const subgraph = useLegacy(
    ['graph', 'subgraph', selected?.id ?? ''],
    `${BASE}/subgraph${selected ? `?node_id=${encodeURIComponent(selected.id)}` : ''}`,
    SubgraphPayloadSchema,
  );
  const canvasNodes = useMemo(() => {
    if (subgraph.data?.outcome !== 'ok') return [];
    return subgraph.data.data.nodes.map((node) => ({
      id: node.id,
      label: node.name ?? node.qualified_name ?? node.id,
      kind: node.kind,
      degree: node.degree ?? 1,
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
          <form
            className="relative"
            onSubmit={(e) => {
              e.preventDefault();
              setSubmitted(query.trim());
            }}
          >
            <Search
              aria-hidden
              size={13}
              className="pointer-events-none absolute left-2 top-1/2 -translate-y-1/2 text-text-muted"
            />
            <input
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="Search symbols"
              aria-label="Symbol search"
              className="h-8 w-full rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-2 pl-7 pr-2 text-xs text-text-primary placeholder:text-text-muted focus:border-accent/60 focus:outline-none"
            />
          </form>
          <LegacyBoundary title="Graph" pending={overview.isPending} result={overview.data}>
            {(data) => {
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
        </div>
      }
      list={
        <div className="flex h-full flex-col">
          <div className="border-b border-edge-subtle p-3">
            {subgraph.isPending ? (
              <p className="p-6 text-center text-sm text-text-muted">
                composing graph neighborhood…
              </p>
            ) : (
              <GraphCanvas
                nodes={canvasNodes}
                edges={canvasEdges}
                selectedId={selected?.id ?? null}
                onSelect={selectFromCanvas}
                height={300}
                activation={activationRef.current}
              />
            )}
          </div>
          <div className="min-h-0 flex-1 overflow-auto">
        {submitted === '' ? (
          <TopConnectedList
            overviewPending={overview.isPending}
            overviewResult={overview.data}
            onSelect={setSelected}
            selected={selected}
          />
        ) : (
          <LegacyBoundary title="Symbols" pending={search.isPending} result={search.data}>
            {(data) => {
              const rows = data.results ?? [];
              if (rows.length === 0)
                return (
                  <p className="p-6 text-center text-sm text-text-muted">
                    no symbols matched “{submitted}”
                  </p>
                );
              const capped = data.total != null && data.total > rows.length;
              const degreeCeiling = rows.reduce(
                (max, node) => Math.max(max, node.degree ?? 0),
                0,
              );
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
                      onSelect={() => setSelected(node)}
                    />
                  )}
                />
              );
            }}
          </LegacyBoundary>
        )}
          </div>
        </div>
      }
      inspector={
        selected ? (
          <InspectorPanel title="Symbol" onClose={() => setSelected(null)}>
            <div className="flex flex-col gap-3">
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

/** Default view when no search is active: the graph's most connected symbols.
 *
 * This used to be thirteen full-width rows — kind chip, name, magnitude rail,
 * path — which is a table, and a table is the one form that throws away
 * everything interesting about this particular data. Every row was the same
 * size, so the fact that the busiest symbol carries nearly five times the
 * connections of the twelfth was encoded only in a 20px bar at the far right
 * of each line; the shape of the drop was invisible; and thirteen identical
 * bands ate roughly sixty percent of the workspace to say one sorted thing.
 *
 * It is replaced by two joined instruments in under half the height:
 *
 *   the SPINE   every hub as a mark on one shared degree axis. Position is
 *               the measurement, so the run-away leader, the steep fall and
 *               the near-ties that bunch at the low end are all directly
 *               visible — a comparison thirteen separate right-aligned bars
 *               could never support. Hue is the symbol's kind, taken from the
 *               very same `kindColor` rule the canvas above paints with, so
 *               the two halves are one system rather than a picture with a
 *               table under it.
 *
 *   the FIELD   the same hubs as compact cards on a dense grid, where the
 *               NAME's type size falls with rank. Magnitude is read as
 *               typography — the busiest symbol is literally the largest text
 *               on the surface — which is the magnitude rail's idea carried
 *               to its conclusion, and it costs no horizontal column at all.
 *
 * Truthfulness: the endpoint (`graph_queries::top_connected_rows`) serves the
 * top twelve rows and exactly five fields — id, name, kind, file_path,
 * degree. So the spine is honestly captioned as those hubs only, its axis is
 * anchored at zero and labelled with the real extremes, and nothing here
 * claims to be the whole graph's degree distribution, which this payload
 * simply does not contain. Search reaches everything else. */
function TopConnectedList({
  overviewPending,
  overviewResult,
  onSelect,
  selected,
}: {
  overviewPending: boolean;
  overviewResult: Parameters<typeof LegacyBoundary>[0]['result'];
  onSelect: (node: GraphNode) => void;
  selected: GraphNode | null;
}) {
  return (
    <LegacyBoundary title="Code" pending={overviewPending} result={overviewResult}>
      {(data) => {
        const payload = data as {
          top_connected?: Array<Record<string, unknown>>;
          totals?: { nodes?: number };
        };
        const hubs = (payload.top_connected ?? []) as GraphNode[];
        if (hubs.length === 0)
          return (
            <p className="p-6 text-center text-sm text-text-muted">
              search the code graph to see symbols
            </p>
          );
        return (
          <HubField
            hubs={hubs}
            indexedNodes={payload.totals?.nodes ?? null}
            onSelect={onSelect}
            selected={selected}
          />
        );
      }}
    </LegacyBoundary>
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
  hubs: GraphNode[];
  indexedNodes: number | null;
  onSelect: (node: GraphNode) => void;
  selected: GraphNode | null;
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
                  title={`${displayName(node)} · ${node.kind} · ${degree.toLocaleString()} deg`}
                  className="absolute top-1/2 rounded-full bg-[var(--kind-dark)] [[data-theme=light]_&]:bg-[var(--kind-light)]"
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
      <ol className="grid grid-cols-1 sm:grid-cols-2 md:grid-cols-3 xl:grid-cols-4">
        {ranked.map((node, rank) => (
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
              rank={rank}
              selected={selected?.id === node.id}
              onSelect={() => onSelect(node)}
            />
          </li>
        ))}
      </ol>
    </div>
  );
}

/** The hub's own name is the headline. `qualified_name` is not served by this
 * endpoint at all, so the fallback chain is honest about what exists. */
function displayName(node: GraphNode | undefined): string {
  if (!node) return '—';
  return node.name ?? node.qualified_name ?? node.id;
}

function HubCard({
  node,
  rank,
  selected,
  onSelect,
}: {
  node: GraphNode;
  rank: number;
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
        'relative flex h-full w-full flex-col gap-0.5 px-3 py-1.5 text-left',
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
          {displayName(node)}
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
      <span className="flex min-w-0 items-baseline gap-2 pl-6 leading-tight">
        <span className="td-legend shrink-0 max-w-24 truncate">{node.kind}</span>
        <span
          className="td-value min-w-0 flex-1 truncate text-right text-3xs text-text-muted"
          title={node.file_path ?? undefined}
        >
          {elideStart(node.file_path, 30)}
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
  node: GraphNode;
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
      <span className="td-value min-w-0 flex-1 truncate text-text-primary">
        {node.qualified_name ?? node.name ?? node.id}
      </span>
      {node.degree != null ? (
        <span className="flex w-20 shrink-0 flex-col items-end gap-1">
          <span
            className="td-value text-2xs leading-none text-text-secondary"
            data-cell="numeric"
          >
            {node.degree}
            <span className="td-unit ml-1">deg</span>
          </span>
          <Meter
            fraction={degreeCeiling > 0 ? node.degree / degreeCeiling : null}
            className="h-[3px] w-full"
            align="right"
          />
        </span>
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
