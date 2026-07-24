import { useState } from 'react';
import { Search } from 'lucide-react';
import { z } from 'zod';
import {
  DataRow,
  ExplorerSplit,
  InspectorPanel,
  KeyValueTree,
} from '../../ui/archetypes/ExplorerSplit.tsx';
import { LegacyBoundary } from '../../ui/LegacyStates.tsx';
import { ActivityColumns } from '../../ui/ActivityColumns.tsx';
import { Meter, Readout } from '../../ui/instrument.tsx';
import { formatStamp, splitCount } from '../../ui/format.ts';
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
            const split = splitCount(total);
            return (
              <div className="flex flex-col gap-3">
                <div className="td-raised border border-edge-subtle px-3 py-3">
                  <Readout
                    label="messages tracked"
                    size="xl"
                    value={split.value}
                    unit={split.unit}
                    note={`${total.toLocaleString()} across ${buckets.length} days`}
                  />
                </div>
                <figure className="flex flex-col gap-1.5">
                  <figcaption className="td-legend">daily volume</figcaption>
                  <ActivityColumns buckets={buckets.slice(-46)} height={56} />
                </figure>
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
                    const when = hit['timestamp'] ? formatStamp(Number(hit['timestamp'])) : '';
                    return (
                      <DataRow
                        key={id}
                        selected={selected === hit}
                        onSelect={() => setSelected(hit)}
                      >
                        <span className="td-legend w-14 shrink-0 truncate">{provider}</span>
                        <span className="td-legend w-14 shrink-0 border border-edge-subtle px-1 py-1 text-center">
                          {role}
                        </span>
                        <span className="min-w-0 flex-1 truncate text-text-primary">
                          {snippet}
                        </span>
                        <span
                          className="td-value w-28 shrink-0 text-right text-2xs text-text-muted"
                          data-cell="numeric"
                        >
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
            // The session list is a list of sizes. Scaling every bar to the
            // heaviest session actually loaded turns thirty near-identical
            // three-digit numbers into a shape the eye can rank.
            const heaviest = rows.reduce(
              (max, row) => Math.max(max, Number(row['message_count'] ?? 0)),
              0,
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
                    ? formatStamp(Number(row['last_timestamp']))
                    : '';
                  return (
                    <DataRow
                      selected={selected === row}
                      onSelect={() => setSelected(row)}
                    >
                      {provider ? (
                        <span className="td-legend w-14 shrink-0 truncate">{provider}</span>
                      ) : null}
                      <span className="td-value min-w-0 flex-1 truncate text-text-primary">
                        {id}
                      </span>
                      {count !== undefined ? (
                        <span className="flex w-24 shrink-0 flex-col items-end gap-1">
                          <span
                            className="td-value text-2xs leading-none text-text-secondary"
                            data-cell="numeric"
                          >
                            {String(count)}
                            <span className="td-unit ml-1">msg</span>
                          </span>
                          <Meter
                            fraction={
                              heaviest > 0 ? Number(count) / heaviest : null
                            }
                            className="h-[3px] w-full"
                            align="right"
                          />
                        </span>
                      ) : null}
                      <span
                        className="td-value w-28 shrink-0 text-right text-2xs text-text-muted"
                        data-cell="numeric"
                      >
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
