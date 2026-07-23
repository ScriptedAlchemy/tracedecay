import { useState } from 'react';
import { z } from 'zod';
import {
  DataRow,
  ExplorerSplit,
  InspectorPanel,
  KeyValueTree,
} from '../../ui/archetypes/ExplorerSplit.tsx';
import { StateChip } from '../../ui/StateChip';
import { AnyObject } from '../../data/query/legacy.ts';
import { useLegacy } from '../../data/query/useLegacy.ts';

const ListPayload = z
  .object({
    results: z.array(AnyObject).optional(),
    items: z.array(AnyObject).optional(),
    nodes: z.array(AnyObject).optional(),
    facts: z.array(AnyObject).optional(),
  })
  .passthrough();

function rowsOf(data: z.infer<typeof ListPayload>): Record<string, unknown>[] {
  return data.results ?? data.items ?? data.nodes ?? data.facts ?? [];
}

const MemoryListPayload = z
  .object({
    holographic: z
      .object({ facts: z.array(AnyObject).optional() })
      .passthrough()
      .optional(),
  })
  .passthrough();

function memoryRows(data: Record<string, unknown>): Record<string, unknown>[] {
  const holographic = data['holographic'];
  if (holographic && typeof holographic === 'object') {
    const facts = (holographic as { facts?: unknown }).facts;
    if (Array.isArray(facts)) return facts as Record<string, unknown>[];
  }
  return [];
}

type SourceResult =
  | { outcome: 'ok'; data: Record<string, unknown> }
  | { outcome: string; data?: unknown };

/** Explorer: one query fanned across independent sources with per-source
 * progress rows (the planner-composer pattern, minimally realized over the
 * legacy search surfaces — the typed PlannerQueryRun replaces this fan-out
 * when plan-09's coordinator is exposed). */
export function ExplorerPage() {
  const [query, setQuery] = useState('');
  const [submitted, setSubmitted] = useState('');
  const [selected, setSelected] = useState<Record<string, unknown> | null>(null);
  const enabled = submitted !== '';

  const graph = useLegacy(
    ['explorer', 'graph', submitted],
    `/api/plugins/graph/search?q=${encodeURIComponent(submitted)}`,
    ListPayload,
  );
  const lcm = useLegacy(
    ['explorer', 'lcm', submitted],
    `/api/plugins/hermes-lcm/search?q=${encodeURIComponent(submitted)}`,
    ListPayload,
  );
  const memory = useLegacy(
    ['explorer', 'memory', submitted],
    `/api/plugins/holographic/?q=${encodeURIComponent(submitted)}&limit=25`,
    MemoryListPayload,
  );

  const sources: Array<{
    name: string;
    query: { isPending: boolean; data?: SourceResult };
    extract: (data: Record<string, unknown>) => Record<string, unknown>[];
  }> = [
    { name: 'code graph', query: graph, extract: rowsOf },
    { name: 'sessions', query: lcm, extract: rowsOf },
    { name: 'knowledge', query: memory, extract: memoryRows },
  ];

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
              placeholder="Search everything…"
              aria-label="Explorer search"
              className="h-8 w-full rounded-[var(--radius-chip)] border border-edge-subtle bg-surface-0 px-2 text-xs outline-none focus-visible:border-accent"
            />
          </form>
          {enabled ? (
            <div className="flex flex-col gap-1.5" aria-label="Source progress">
              {sources.map((s) => (
                <div key={s.name} className="flex items-center justify-between text-2xs">
                  <span className="text-text-muted">{s.name}</span>
                  {s.query.isPending ? (
                    <StateChip kind="loading" />
                  ) : s.query.data?.outcome === 'ok' ? (
                    <span className="tabular text-text-secondary">
                      {s.extract(s.query.data.data as Record<string, unknown>).length}
                    </span>
                  ) : (
                    <StateChip
                      kind={s.query.data?.outcome === 'offline' ? 'offline' : 'error'}
                    />
                  )}
                </div>
              ))}
            </div>
          ) : null}
        </div>
      }
      list={
        !enabled ? (
          <p className="p-6 text-center text-sm text-text-muted">
            search across sessions, code, and knowledge
          </p>
        ) : (
          <div>
            {sources.map((s) =>
              s.query.data?.outcome === 'ok'
                ? s.extract(s.query.data.data as Record<string, unknown>).map((row, i) => {
                    const label = String(
                      row['qualified_name'] ??
                        row['name'] ??
                        row['summary'] ??
                        row['content'] ??
                        row['text'] ??
                        row['session_id'] ??
                        i,
                    );
                    return (
                      <DataRow
                        key={`${s.name}-${i}`}
                        selected={selected === row}
                        onSelect={() => setSelected(row)}
                      >
                        <span className="w-24 shrink-0 truncate text-2xs text-text-muted">
                          {s.name}
                        </span>
                        <span className="min-w-0 flex-1 truncate">{label}</span>
                      </DataRow>
                    );
                  })
                : null,
            )}
          </div>
        )
      }
      inspector={
        selected ? (
          <InspectorPanel title="Result" onClose={() => setSelected(null)}>
            <KeyValueTree value={selected} />
          </InspectorPanel>
        ) : undefined
      }
    />
  );
}
