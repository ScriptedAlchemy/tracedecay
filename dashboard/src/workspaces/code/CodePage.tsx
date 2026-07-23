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
import { useLegacy } from '../../data/query/useLegacy.ts';
import {
  GraphOverviewPayloadSchema,
  GraphSearchPayloadSchema,
  type GraphNode,
} from './contracts.ts';

const BASE = '/api/plugins/graph';

/** Code: graph overview (kind composition), symbol search, node inspector.
 * The Sigma canvas over the subgraph endpoint is the phase-2 renderer per the
 * visualization catalog. */
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
                    <StatTile label="nodes" value={data.totals.nodes.toLocaleString()} />
                    <StatTile label="edges" value={data.totals.edges.toLocaleString()} />
                    <StatTile label="files" value={data.totals.files.toLocaleString()} />
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
        submitted === '' ? (
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
              return (
                <div>
                  <p className="border-b border-edge-subtle px-3 py-1.5 text-2xs text-text-muted">
                    {data.total ?? rows.length} matches
                  </p>
                  {rows.map((node) => (
                    <SymbolRow
                      key={node.id}
                      node={node}
                      selected={selected?.id === node.id}
                      onSelect={() => setSelected(node)}
                    />
                  ))}
                </div>
              );
            }}
          </LegacyBoundary>
        )
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
          <div>
            <p className="border-b border-edge-subtle px-3 py-1.5 text-2xs text-text-muted">
              most connected symbols
            </p>
            {hubs.map((row, i) => {
              const node = row as GraphNode;
              return (
                <SymbolRow
                  key={String(node.id ?? i)}
                  node={node}
                  selected={selected?.id === node.id}
                  onSelect={() => onSelect(node)}
                />
              );
            })}
          </div>
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
