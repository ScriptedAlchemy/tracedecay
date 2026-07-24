import { useState } from 'react';
import { Search } from 'lucide-react';
import { z } from 'zod';
import {
  DataRow,
  ExplorerSplit,
  InspectorPanel,
  KeyValueTree,
} from '../../ui/archetypes/ExplorerSplit.tsx';
import { LegacyBoundary, StatTile } from '../../ui/LegacyStates.tsx';
import { ActivityColumns } from '../../ui/ActivityColumns.tsx';
import { VirtualList } from '../../ui/VirtualList.tsx';
import { AnyObject } from '../../data/query/legacy.ts';
import { useLegacy } from '../../data/query/useLegacy.ts';

const BASE = '/api/plugins/hermes-lcm';

const OverviewPayload = z
  .object({ latest_sessions: z.array(AnyObject).optional() })
  .passthrough();
const TimelinePayload = z
  .object({ buckets: z.array(AnyObject).optional() })
  .passthrough();
/** Wire-true transcript search (lcm_api.rs search): hits nest under
 * matches.messages / matches.summary_nodes. */
const SearchPayload = z
  .object({
    matches: z
      .object({
        messages: z.array(AnyObject).optional(),
        summary_nodes: z.array(AnyObject).optional(),
      })
      .passthrough()
      .optional(),
    total: z
      .object({ messages: z.number().optional(), summary_nodes: z.number().optional() })
      .passthrough()
      .optional(),
  })
  .passthrough();

/** Sessions: LCM store — overview stats, transcript search across every
 * provider, session list, and drill-down. */
export function SessionsPage() {
  const overview = useLegacy(['lcm', 'overview'], `${BASE}/overview`, OverviewPayload);
  const timeline = useLegacy(['lcm', 'timeline'], `${BASE}/timeline`, TimelinePayload);
  const [selected, setSelected] = useState<Record<string, unknown> | null>(null);
  const [query, setQuery] = useState('');
  const [submitted, setSubmitted] = useState('');
  const search = useLegacy(
    ['lcm', 'search', submitted],
    `${BASE}/search?q=${encodeURIComponent(submitted)}&limit=50`,
    SearchPayload,
    { enabled: submitted !== '' },
  );

  return (
    <ExplorerSplit
      filters={
        <div className="flex flex-col gap-3">
          <form
            className="relative"
            onSubmit={(event) => {
              event.preventDefault();
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
              onChange={(event) => setQuery(event.target.value)}
              placeholder="Search transcripts"
              aria-label="Search transcripts"
              className="h-8 w-full rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-2 pl-7 pr-2 text-xs text-text-primary placeholder:text-text-muted focus:border-accent/60 focus:outline-none"
            />
          </form>
        <LegacyBoundary title="LCM" pending={timeline.isPending} result={timeline.data}>
          {(data) => {
            const buckets = (data.buckets ?? []).map((b) => ({
              label: String(b['bucket'] ?? ''),
              value: Number(b['count'] ?? 0),
              hint: `~${Number(b['token_estimate'] ?? 0).toLocaleString()} tokens`,
            }));
            const total = buckets.reduce((sum, b) => sum + b.value, 0);
            return (
              <div className="flex flex-col gap-3">
                <ActivityColumns buckets={buckets.slice(-46)} />
                <StatTile label="messages tracked" value={total.toLocaleString()} />
              </div>
            );
          }}
        </LegacyBoundary>
        </div>
      }
      list={
        submitted !== '' ? (
          <LegacyBoundary title="Transcript search" pending={search.isPending} result={search.data}>
            {(data) => {
              const hits = data.matches?.messages ?? [];
              if (hits.length === 0)
                return (
                  <p className="p-6 text-center text-sm text-text-muted">
                    no transcript messages match “{submitted}”
                  </p>
                );
              return (
                <div>
                  <p className="tabular border-b border-edge-subtle px-3 py-1.5 text-2xs text-text-muted">
                    {data.total?.messages ?? hits.length} message matches
                    {data.total?.summary_nodes
                      ? ` · ${data.total.summary_nodes} summaries`
                      : ''}
                  </p>
                  {hits.map((hit, i) => {
                    const id = String(hit['message_id'] ?? hit['store_id'] ?? i);
                    const provider = String(hit['source'] ?? hit['provider'] ?? '');
                    const role = String(hit['role'] ?? '');
                    const snippet = String(hit['snippet'] ?? hit['content'] ?? '');
                    const when = hit['timestamp']
                      ? new Date(Number(hit['timestamp']) * 1000).toLocaleString()
                      : '';
                    return (
                      <DataRow
                        key={id}
                        selected={selected === hit}
                        onSelect={() => setSelected(hit)}
                      >
                        <span className="w-14 shrink-0 truncate text-2xs text-text-muted">
                          {provider}
                        </span>
                        <span className="w-14 shrink-0 rounded-[var(--radius-chip)] border border-edge-subtle px-1 text-center text-2xs text-text-muted">
                          {role}
                        </span>
                        <span className="min-w-0 flex-1 truncate">{snippet}</span>
                        <span className="tabular hidden shrink-0 text-2xs text-text-muted sm:inline">
                          {when}
                        </span>
                      </DataRow>
                    );
                  })}
                </div>
              );
            }}
          </LegacyBoundary>
        ) : (
        <LegacyBoundary title="Sessions" pending={overview.isPending} result={overview.data}>
          {(data) => {
            const rows = data.latest_sessions ?? [];
            if (rows.length === 0)
              return (
                <p className="p-6 text-center text-sm text-text-muted">
                  no sessions in the current window
                </p>
              );
            return (
              <VirtualList
                items={rows}
                getKey={(row, i) => String(row['session_id'] ?? row['id'] ?? i)}
                renderItem={(row, i) => {
                  const id = String(row['session_id'] ?? row['id'] ?? i);
                  const provider = String(row['provider'] ?? row['source'] ?? '');
                  const count = row['message_count'];
                  const when = row['last_timestamp']
                    ? new Date(Number(row['last_timestamp']) * 1000).toLocaleString()
                    : '';
                  return (
                    <DataRow
                      selected={selected === row}
                      onSelect={() => setSelected(row)}
                    >
                      {provider ? (
                        <span className="w-24 shrink-0 truncate text-text-muted">{provider}</span>
                      ) : null}
                      <span className="min-w-0 flex-1 truncate font-mono">{id}</span>
                      {count !== undefined ? (
                        <span className="tabular shrink-0 text-2xs text-text-muted">
                          {String(count)} msgs
                        </span>
                      ) : null}
                      {/* The timestamp is the widest fixed-width field in this
                       * row; at 320px it and the message count alone left no
                       * room at all for the session id (the actually-useful
                       * identifier), so the id rendered as an empty sliver.
                       * Drop the timestamp below `sm` rather than let it win
                       * that fight every time. */}
                      <span className="tabular hidden shrink-0 text-2xs text-text-muted sm:inline">
                        {when}
                      </span>
                    </DataRow>
                  );
                }}
              />
            );
          }}
        </LegacyBoundary>
        )
      }
      inspector={
        selected ? (
          <InspectorPanel title="Session" onClose={() => setSelected(null)}>
            <KeyValueTree value={selected} />
          </InspectorPanel>
        ) : undefined
      }
    />
  );
}
