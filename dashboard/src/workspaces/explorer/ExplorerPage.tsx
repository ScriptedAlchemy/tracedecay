import { useState } from 'react';
import { Search } from 'lucide-react';
import { z } from 'zod';
import {
  DataRow,
  ExplorerSplit,
  InspectorPanel,
  KeyValueTree,
} from '../../ui/archetypes/ExplorerSplit.tsx';
import { StateChip } from '../../ui/StateChip';
import { VirtualList } from '../../ui/VirtualList.tsx';
import { Meter } from '../../ui/instrument.tsx';
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
      .object({
        facts: z.array(AnyObject).optional(),
        entities: z
          .array(
            z
              .object({ name: z.string(), fact_count: z.number().optional() })
              .passthrough(),
          )
          .optional(),
      })
      .passthrough()
      .optional(),
  })
  .passthrough();

const LcmSearchPayload = z
  .object({
    matches: z
      .object({
        messages: z.array(AnyObject).optional(),
        summary_nodes: z.array(AnyObject).optional(),
      })
      .passthrough(),
  })
  .passthrough();

function lcmRows(data: Record<string, unknown>): Record<string, unknown>[] {
  const matches = data['matches'] as z.infer<typeof LcmSearchPayload>['matches'];
  return [...(matches.messages ?? []), ...(matches.summary_nodes ?? [])];
}

function memoryRows(data: Record<string, unknown>): Record<string, unknown>[] {
  const holographic = data['holographic'];
  if (holographic && typeof holographic === 'object') {
    const facts = (holographic as { facts?: unknown }).facts;
    if (Array.isArray(facts)) return facts as Record<string, unknown>[];
  }
  return [];
}

/** `/api/plugins/graph/overview` — the code index's own size and its most
 * connected symbols. One of the two cheap reads the idle state is composed
 * from (~40ms against a live daemon). */
const GraphIndexPayload = z
  .object({
    totals: z
      .object({ nodes: z.number(), edges: z.number(), files: z.number() })
      .passthrough(),
    top_connected: z
      .array(
        z
          .object({
            id: z.string().optional(),
            name: z.string().nullable().optional(),
            kind: z.string().optional(),
            file_path: z.string().nullable().optional(),
            degree: z.number().optional(),
          })
          .passthrough(),
      )
      .optional(),
  })
  .passthrough();

/** `/api/plugins/holographic/status` — the memory store's size (~110ms). */
const MemoryIndexPayload = z
  .object({
    exists: z.boolean().optional(),
    memory: z
      .object({
        fact_count: z.number().optional(),
        entity_count: z.number().optional(),
        bank_count: z.number().optional(),
      })
      .passthrough()
      .optional(),
  })
  .passthrough();

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
    LcmSearchPayload,
  );
  const memory = useLegacy(
    ['explorer', 'memory', submitted],
    `/api/plugins/holographic/?q=${encodeURIComponent(submitted)}&limit=25`,
    MemoryListPayload,
  );

  // The idle state's two reads. Both are index summaries rather than searches,
  // both answer in well under a second, and neither is issued once a query is
  // running — the surface is then about the results, not about the index.
  const graphIndex = useLegacy(
    ['explorer', 'graph-index'],
    '/api/plugins/graph/overview',
    GraphIndexPayload,
    { enabled: !enabled },
  );
  const memoryIndex = useLegacy(
    ['explorer', 'memory-index'],
    '/api/plugins/holographic/status',
    MemoryIndexPayload,
    { enabled: !enabled },
  );
  const memoryEntities = useLegacy(
    ['explorer', 'memory-entities'],
    '/api/plugins/holographic/?limit=25',
    MemoryListPayload,
    { enabled: !enabled },
  );

  const runQuery = (next: string) => {
    setQuery(next);
    setSubmitted(next);
  };

  const sources: Array<{
    name: string;
    query: { isPending: boolean; data?: SourceResult };
    extract: (data: Record<string, unknown>) => Record<string, unknown>[];
  }> = [
    { name: 'code graph', query: graph, extract: rowsOf },
    { name: 'sessions', query: lcm, extract: lcmRows },
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
          ) : (
            <IndexSummary
              graph={graphIndex.data?.outcome === 'ok' ? graphIndex.data.data : undefined}
              graphPending={graphIndex.isPending}
              memory={
                memoryIndex.data?.outcome === 'ok' ? memoryIndex.data.data : undefined
              }
              memoryPending={memoryIndex.isPending}
            />
          )}
        </div>
      }
      list={
        !enabled ? (
          <EmptyQuery
            onRun={runQuery}
            hubs={
              graphIndex.data?.outcome === 'ok'
                ? (graphIndex.data.data.top_connected ?? [])
                : []
            }
            entities={
              memoryEntities.data?.outcome === 'ok'
                ? (memoryEntities.data.data.holographic?.entities ?? [])
                : []
            }
          />
        ) : (
          <VirtualList
            items={sources.flatMap((s) =>
              s.query.data?.outcome === 'ok'
                ? s
                    .extract(s.query.data.data as Record<string, unknown>)
                    .map((row, i) => ({ source: s.name, row, index: i }))
                : [],
            )}
            getKey={(entry) => `${entry.source}-${entry.index}`}
            renderItem={(entry) => {
              const { source, row } = entry;
              const label = String(
                row['qualified_name'] ??
                  row['name'] ??
                  row['summary'] ??
                  row['content'] ??
                  row['text'] ??
                  row['session_id'] ??
                  entry.index,
              );
              return (
                <DataRow
                  selected={selected === row}
                  onSelect={() => setSelected(row)}
                >
                  <span className="w-24 shrink-0 truncate text-2xs text-text-muted">
                    {source}
                  </span>
                  <span className="min-w-0 flex-1 truncate">{label}</span>
                </DataRow>
              );
            }}
          />
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

/**
 * What each source actually holds, in the rail, before a query exists.
 *
 * The idle rail used to show nothing but an input. Two summary endpoints
 * answer in under a fifth of a second between them and report the real size of
 * two of the three sources, so the reader can see what they are about to
 * search rather than take it on faith. The third — sessions — has no index
 * summary route that answers reliably, and that is stated rather than filled
 * with a plausible number.
 */
function IndexSummary({
  graph,
  graphPending,
  memory,
  memoryPending,
}: {
  graph: z.infer<typeof GraphIndexPayload> | undefined;
  graphPending: boolean;
  memory: z.infer<typeof MemoryIndexPayload> | undefined;
  memoryPending: boolean;
}) {
  return (
    <figure className="flex flex-col gap-2">
      <figcaption className="td-legend">what is indexed</figcaption>
      <SourceSize
        name="code graph"
        pending={graphPending}
        parts={
          graph
            ? [
                [graph.totals.nodes, 'symbols'],
                [graph.totals.edges, 'edges'],
                [graph.totals.files, 'files'],
              ]
            : null
        }
      />
      <SourceSize
        name="knowledge"
        pending={memoryPending}
        parts={
          memory?.memory
            ? [
                [memory.memory.fact_count ?? 0, 'facts'],
                [memory.memory.entity_count ?? 0, 'entities'],
                [memory.memory.bank_count ?? 0, 'banks'],
              ]
            : null
        }
      />
      <div className="flex flex-col gap-0.5">
        <span className="text-2xs text-text-secondary">sessions</span>
        <span className="text-3xs leading-relaxed text-text-muted">
          searched live — the daemon serves no size for this source, so none is shown
        </span>
      </div>
    </figure>
  );
}

function SourceSize({
  name,
  pending,
  parts,
}: {
  name: string;
  pending: boolean;
  parts: ReadonlyArray<readonly [number, string]> | null;
}) {
  return (
    <div className="flex flex-col gap-0.5">
      <span className="text-2xs text-text-secondary">{name}</span>
      {pending ? (
        <StateChip kind="loading" />
      ) : parts ? (
        <span className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5">
          {parts.map(([value, unit]) => (
            <span key={unit} className="td-value text-3xs text-text-primary" data-cell="numeric">
              {value.toLocaleString()}
              <span className="td-unit ml-1">{unit}</span>
            </span>
          ))}
        </span>
      ) : (
        <span className="text-3xs text-text-muted">index size unavailable</span>
      )}
    </div>
  );
}

/**
 * The query-before-results state, composed from what the daemon already knows
 * rather than left as one sentence in a void.
 *
 * Everything here is a real read and everything here is a way in: the code
 * graph's most connected symbols and the entities the memory store holds the
 * most facts about, each one running itself as a query when pressed. Nothing
 * is invented — if a source did not answer, its column simply is not drawn.
 */
function EmptyQuery({
  onRun,
  hubs,
  entities,
}: {
  onRun: (query: string) => void;
  hubs: ReadonlyArray<{ name?: string | null; kind?: string; degree?: number }>;
  entities: ReadonlyArray<{ name: string; fact_count?: number }>;
}) {
  const topHubs = hubs.filter((hub) => hub.name).slice(0, 8);
  const topEntities = entities.slice(0, 8);
  const hubCeiling = topHubs.reduce((max, hub) => Math.max(max, hub.degree ?? 0), 0);
  const entityCeiling = topEntities.reduce(
    (max, entity) => Math.max(max, entity.fact_count ?? 0),
    0,
  );
  return (
    // Bounded width on purpose. These are two ranked columns of eight rows;
    // let loose across a 1440px workspace each row becomes a name at the far
    // left and its measurements at the far right with a hand's width of nothing
    // between them, which is not density, only distance.
    <div className="flex h-full w-full max-w-3xl flex-col gap-5 overflow-auto p-6">
      <div className="flex items-start gap-3">
        <span
          aria-hidden
          className="flex size-9 shrink-0 items-center justify-center rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-1 text-text-muted"
        >
          <Search size={17} />
        </span>
        <div className="flex min-w-0 flex-col gap-1">
          <h2 className="text-sm font-semibold tracking-tight">Search across every surface</h2>
          <p className="text-xs leading-relaxed text-text-muted">
            One query fans out to the code graph, sessions and knowledge at once, each
            with its own progress in the rail. The rail also carries what each of them
            currently holds.
          </p>
        </div>
      </div>

      {topHubs.length > 0 || topEntities.length > 0 ? (
        <div className="grid gap-x-6 gap-y-5 md:grid-cols-2">
          {topHubs.length > 0 ? (
            <SeedList
              legend={`most connected symbols · top ${topHubs.length} by degree`}
              rows={topHubs.map((hub) => ({
                label: String(hub.name),
                note: hub.kind ?? '',
                value: hub.degree ?? 0,
                unit: 'deg',
              }))}
              ceiling={hubCeiling}
              onRun={onRun}
            />
          ) : null}
          {topEntities.length > 0 ? (
            <SeedList
              legend={`entities by fact count · top ${topEntities.length}`}
              rows={topEntities.map((entity) => ({
                label: entity.name,
                note: '',
                value: entity.fact_count ?? 0,
                unit: 'facts',
              }))}
              ceiling={entityCeiling}
              onRun={onRun}
            />
          ) : null}
        </div>
      ) : (
        <p className="text-2xs text-text-muted">
          Neither index answered, so there is nothing here to offer as a starting point.
          The search above still works.
        </p>
      )}
    </div>
  );
}

/** A ranked column of real index entries, each one a query waiting to be run. */
function SeedList({
  legend,
  rows,
  ceiling,
  onRun,
}: {
  legend: string;
  rows: ReadonlyArray<{ label: string; note: string; value: number; unit: string }>;
  ceiling: number;
  onRun: (query: string) => void;
}) {
  return (
    <figure className="flex min-w-0 flex-col gap-1.5">
      <figcaption className="td-legend">{legend}</figcaption>
      <ul className="flex flex-col">
        {rows.map((row, index) => (
          <li key={`${row.label}-${index}`}>
            <button
              type="button"
              onClick={() => onRun(row.label)}
              className="flex w-full items-center gap-2 border-b border-edge-subtle py-1.5 text-left last:border-b-0 hover:bg-surface-1 focus-visible:bg-surface-1"
            >
              <span className="td-value min-w-0 flex-1 truncate text-xs text-text-primary">
                {row.label}
              </span>
              {row.note ? (
                <span className="td-legend shrink-0 max-w-20 truncate max-sm:hidden">
                  {row.note}
                </span>
              ) : null}
              <Meter
                fraction={ceiling > 0 ? row.value / ceiling : null}
                className="h-[3px] w-14 shrink-0 max-sm:hidden"
              />
              <span
                className="td-value w-14 shrink-0 text-right text-2xs text-text-secondary"
                data-cell="numeric"
              >
                {row.value.toLocaleString()}
                <span className="td-unit ml-1">{row.unit}</span>
              </span>
            </button>
          </li>
        ))}
      </ul>
    </figure>
  );
}
