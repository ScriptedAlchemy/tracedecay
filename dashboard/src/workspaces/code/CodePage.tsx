import { useState } from 'react';
import { Search } from 'lucide-react';
import {
  DataRow,
  ExplorerSplit,
  InspectorPanel,
  KeyValueTree,
} from '../../ui/archetypes/ExplorerSplit.tsx';
import { LegacyBoundary, StatTile } from '../../ui/LegacyStates.tsx';
import { ActivityColumns } from '../../ui/ActivityColumns.tsx';
import { VirtualList } from '../../ui/VirtualList.tsx';
import { formatCount } from '../../ui/format.ts';
import { useLegacy } from '../../data/query/useLegacy.ts';
import { useCallback, useEffect, useMemo, useRef } from 'react';
import { GraphCanvas } from '../../viz/graph/GraphCanvas.tsx';
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
                  <div className="grid grid-cols-3 gap-2">
                    <StatTile dense label="nodes" value={formatCount(data.totals.nodes)} />
                    <StatTile dense label="edges" value={formatCount(data.totals.edges)} />
                    <StatTile dense label="files" value={formatCount(data.totals.files)} />
                  </div>
                  {kinds.length > 0 ? (
                    <figure className="flex flex-col gap-1">
                      <figcaption className="text-2xs text-text-muted">
                        node composition by kind
                      </figcaption>
                      <ActivityColumns buckets={kinds} height={40} />
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
              return (
                <VirtualList
                  items={rows}
                  getKey={(node) => node.id}
                  header={
                    <p className="border-b border-edge-subtle px-3 py-1.5 text-2xs text-text-muted">
                      {capped
                        ? `${rows.length} of ${data.total} matches`
                        : `${data.total ?? rows.length} matches`}
                    </p>
                  }
                  renderItem={(node) => (
                    <SymbolRow
                      node={node}
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

/** Default list when no search is active: the graph's most connected symbols,
 * so the workspace opens onto structure instead of an empty prompt. */
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
        const payload = data as { top_connected?: Array<Record<string, unknown>> };
        const hubs = payload.top_connected ?? [];
        if (hubs.length === 0)
          return (
            <p className="p-6 text-center text-sm text-text-muted">
              search the code graph to see symbols
            </p>
          );
        return (
          <VirtualList
            items={hubs}
            getKey={(row, i) => String((row as GraphNode).id ?? i)}
            header={
              <p className="border-b border-edge-subtle px-3 py-1.5 text-2xs text-text-muted">
                most connected symbols
              </p>
            }
            renderItem={(row) => {
              const node = row as GraphNode;
              return (
                <SymbolRow
                  node={node}
                  selected={selected?.id === node.id}
                  onSelect={() => onSelect(node)}
                />
              );
            }}
          />
        );
      }}
    </LegacyBoundary>
  );
}

function SymbolRow({
  node,
  selected,
  onSelect,
}: {
  node: GraphNode;
  selected: boolean;
  onSelect: () => void;
}) {
  return (
    <DataRow selected={selected} onSelect={onSelect}>
      <span className="w-20 shrink-0 truncate text-2xs text-text-muted">{node.kind}</span>
      <span className="min-w-0 flex-1 truncate font-mono">
        {node.qualified_name ?? node.name ?? node.id}
      </span>
      {node.degree != null ? (
        <span className="tabular w-14 shrink-0 text-right text-2xs text-text-muted">
          {node.degree} deg
        </span>
      ) : null}
      <span className="w-44 shrink-0 truncate text-right font-mono text-2xs text-text-muted">
        {node.file_path ?? ''}
      </span>
    </DataRow>
  );
}
