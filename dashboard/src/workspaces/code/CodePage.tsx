import { useState } from 'react';
import { z } from 'zod';
import {
  DataRow,
  ExplorerSplit,
  InspectorPanel,
  KeyValueTree,
} from '../../ui/archetypes/ExplorerSplit.tsx';
import { LegacyBoundary, StatTile } from '../../ui/LegacyStates.tsx';
import { AnyObject } from '../../data/query/legacy.ts';
import { useLegacy } from '../../data/query/useLegacy.ts';

const BASE = '/api/plugins/graph';

const SearchPayload = z
  .object({ results: z.array(AnyObject).optional(), nodes: z.array(AnyObject).optional() })
  .passthrough();

/** Code: graph overview + symbol search + node inspector. The Sigma canvas
 * over ProjectionView is the phase-2 renderer per the catalog. */
export function CodePage() {
  const overview = useLegacy(['graph', 'overview'], `${BASE}/overview`, AnyObject);
  const [query, setQuery] = useState('');
  const [submitted, setSubmitted] = useState('');
  const search = useLegacy(
    ['graph', 'search', submitted],
    `${BASE}/search?q=${encodeURIComponent(submitted)}`,
    SearchPayload,
  );
  const [selected, setSelected] = useState<Record<string, unknown> | null>(null);

  return (
    <ExplorerSplit
      filters={
        <div className="flex flex-col gap-3">
          <form
            onSubmit={(e) => {
              e.preventDefault();
              setSubmitted(query);
            }}
          >
            <input
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="Search symbols…"
              aria-label="Symbol search"
              className="h-8 w-full rounded-[var(--radius-chip)] border border-edge-subtle bg-surface-0 px-2 text-xs outline-none focus-visible:border-accent"
            />
          </form>
          <LegacyBoundary title="Graph" pending={overview.isPending} result={overview.data}>
            {(data) => (
              <div className="flex flex-col gap-2">
                {Object.entries(data)
                  .filter(([, v]) => typeof v === 'number')
                  .slice(0, 8)
                  .map(([k, v]) => (
                    <StatTile key={k} label={k.replaceAll('_', ' ')} value={String(v)} />
                  ))}
              </div>
            )}
          </LegacyBoundary>
        </div>
      }
      list={
        submitted === '' ? (
          <p className="p-6 text-center text-sm text-text-muted">
            search the code graph to see symbols
          </p>
        ) : (
          <LegacyBoundary title="Code" pending={search.isPending} result={search.data}>
            {(data) => {
              const rows = data.results ?? data.nodes ?? [];
              if (rows.length === 0)
                return (
                  <p className="p-6 text-center text-sm text-text-muted">
                    no symbols matched “{submitted}”
                  </p>
                );
              return (
                <div>
                  {rows.map((row, i) => {
                    const name = String(row['qualified_name'] ?? row['name'] ?? i);
                    const kind = String(row['kind'] ?? '');
                    const file = String(row['file_path'] ?? row['path'] ?? '');
                    return (
                      <DataRow
                        key={`${name}-${i}`}
                        selected={selected === row}
                        onSelect={() => setSelected(row)}
                      >
                        <span className="w-20 shrink-0 truncate text-2xs text-text-muted">
                          {kind}
                        </span>
                        <span className="min-w-0 flex-1 truncate font-mono">{name}</span>
                        <span className="w-40 shrink-0 truncate text-2xs text-text-muted">
                          {file}
                        </span>
                      </DataRow>
                    );
                  })}
                </div>
              );
            }}
          </LegacyBoundary>
        )
      }
      inspector={
        selected ? (
          <InspectorPanel title="Symbol" onClose={() => setSelected(null)}>
            <KeyValueTree value={selected} />
          </InspectorPanel>
        ) : undefined
      }
    />
  );
}
