import { useState } from 'react';
import { z } from 'zod';
import {
  DataRow,
  ExplorerSplit,
  InspectorPanel,
  KeyValueTree,
} from '../../ui/archetypes/ExplorerSplit.tsx';
import { LegacyBoundary } from '../../ui/ReadSection.tsx';
import { ActivityColumns } from '../../ui/ActivityColumns.tsx';
import { FigureRail, Readout } from '../../ui/instrument.tsx';
import { SearchField } from '../../ui/search/SearchField.tsx';
import { formatStamp, splitCount } from '../../ui/format.ts';
import { VirtualList } from '../../ui/VirtualList.tsx';
import { AnyObject } from '../../data/query/legacy.ts';
import { useLegacy } from '../../data/query/useLegacy.ts';
import { LcmTimelinePayloadV1Schema } from '../../contracts/generated.ts';
import { SessionInspector } from './SessionInspector.tsx';

const BASE = '/api/plugins/hermes-lcm';

/**
 * UNCONTRACTED. `lcm_api.rs::overview` and `::search` still answer with
 * `serde_json::Value`, so these two are read structurally. Every other route
 * this workspace touches is generated; when those two gain a schemars DTO these
 * go with them.
 */
const OverviewPayload = z
  .object({ exists: z.boolean().optional(), latest_sessions: z.array(AnyObject).optional() })
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
  const timeline = useLegacy(['lcm', 'timeline'], `${BASE}/timeline`, LcmTimelinePayloadV1Schema);
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
      header={
        <div className="border-b border-edge-subtle bg-surface-1 px-4 py-2">
          <h1 className="text-sm font-semibold tracking-tight">Sessions</h1>
        </div>
      }
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
            label="Search transcripts"
            placeholder="Search transcripts"
            hint="press / to focus, Esc to clear"
            submitted={submitted}
          />
        <LegacyBoundary title="LCM" pending={timeline.isPending} result={timeline.data}>
          {(data) => {
            if (data.exists === false) {
              return (
                <p className="text-2xs leading-relaxed text-text-muted">
                  LCM session store is unavailable; message volume is unknown.
                </p>
              );
            }
            const buckets = data.buckets.map((b) => ({
              label: b.bucket,
              value: b.count,
              hint: `~${b.token_estimate.toLocaleString()} tokens`,
            }));
            const total = buckets.reduce((sum, b) => sum + b.value, 0);
            const split = splitCount(total);
            const coverage = data.coverage;
            // `undated` is still a bare map on the Rust side, so its one known
            // key is read rather than typed. A non-numeric value means the
            // daemon reported something this build cannot count, which is not
            // the same as counting zero — so the line below stays unrendered
            // rather than claiming there are no undated messages.
            const undatedCount = data.undated['count'];
            const undated = typeof undatedCount === 'number' ? undatedCount : null;
            return (
              <div className="flex flex-col gap-3">
                <div className="td-raised border border-edge-subtle px-3 py-3">
                  <Readout
                    label="messages in loaded recent window"
                    size="xl"
                    value={split.value}
                    unit={split.unit}
                    note={`${total.toLocaleString()} in ${buckets.length} loaded recent days`}
                  />
                </div>
                <figure className="flex flex-col gap-1.5">
                  <figcaption className="td-legend">recent daily volume</figcaption>
                  <ActivityColumns buckets={buckets.slice(-46)} height={56} />
                </figure>
                {coverage ? (
                  <p className="text-3xs leading-relaxed text-text-muted">
                    {coverage.returned_buckets.toLocaleString()} of{' '}
                    {coverage.total_dated_buckets.toLocaleString()} dated day buckets loaded
                    (limit {coverage.limit.toLocaleString()})
                    {coverage.truncated ? '; older days omitted' : ''}.
                  </p>
                ) : (
                  <p className="text-3xs leading-relaxed text-text-secondary">
                    Timeline coverage was not reported.
                  </p>
                )}
                {undated != null && undated > 0 ? (
                  <p className="text-3xs leading-relaxed text-text-muted">
                    {undated.toLocaleString()} undated messages are separate from this chart.
                  </p>
                ) : null}
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
                    const provider = String(hit['source'] ?? hit['provider'] ?? '');
                    const storeId = hit['store_id'];
                    const messageId = hit['message_id'];
                    const id =
                      storeId != null
                        ? String(storeId)
                        : messageId != null
                          ? `${provider}:${String(messageId)}`
                          : String(i);
                    const role = String(hit['role'] ?? '');
                    const snippet = String(hit['snippet'] ?? hit['content'] ?? '');
                    const when = hit['timestamp'] ? formatStamp(Number(hit['timestamp'])) : '';
                    const selectedProvider = String(
                      selected?.['source'] ?? selected?.['provider'] ?? '',
                    );
                    return (
                      <DataRow
                        key={id}
                        selected={
                          selected != null &&
                          (storeId != null
                            ? selected['store_id'] === storeId
                            : messageId != null &&
                              selected['message_id'] === messageId &&
                              selectedProvider === provider)
                        }
                        onSelect={() => setSelected(hit)}
                      >
                        <span className="td-legend w-14 shrink-0 truncate max-md:hidden">
                          {provider}
                        </span>
                        <span className="td-legend w-14 shrink-0 border border-edge-subtle px-1 py-1 text-center">
                          {role}
                        </span>
                        <span className="min-w-0 flex-1 truncate text-text-primary">
                          {snippet}
                        </span>
                        <span
                          className="td-value w-28 shrink-0 whitespace-nowrap text-right text-2xs text-text-muted max-md:hidden"
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
            if (data.exists === false) {
              return (
                <p className="p-6 text-center text-sm text-text-muted">
                  LCM session store is unavailable; session count is unknown.
                </p>
              );
            }
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
                      selected={
                        selected != null &&
                        ((row['session_id'] != null &&
                          selected['session_id'] === row['session_id']) ||
                          (row['session_id'] == null &&
                            row['id'] != null &&
                            selected['id'] === row['id']))
                      }
                      onSelect={() => setSelected(row)}
                    >
                      {/* The session id carries the provider as its own prefix,
                       * so under 768px the separate provider column and the
                       * wall-clock stamp both go: keeping them collapsed the
                       * id -- the only unique thing on the row -- to nothing. */}
                      {provider ? (
                        <span className="td-legend w-14 shrink-0 truncate max-md:hidden">
                          {provider}
                        </span>
                      ) : null}
                      <span className="td-value min-w-0 flex-1 truncate text-text-primary">
                        {id}
                      </span>
                      {count !== undefined ? (
                        <FigureRail
                          value={String(count)}
                          unit="msg"
                          width="wide"
                          fraction={heaviest > 0 ? Number(count) / heaviest : null}
                        />
                      ) : null}
                      <span
                        className="td-value w-28 shrink-0 whitespace-nowrap text-right text-2xs text-text-muted max-md:hidden"
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
          <SelectedRecord record={selected} onClose={() => setSelected(null)} />
        ) : undefined
      }
    />
  );
}

/**
 * The inspector for whatever the list selected.
 *
 * Both the session list and a transcript search hit identify a session, so both
 * open the real transcript drill-down. A row that carries no session id at all
 * still gets its exact record rather than an empty panel — the point is never
 * to show less than the store served.
 */
function SelectedRecord({
  record,
  onClose,
}: {
  record: Record<string, unknown> | null;
  onClose: () => void;
}) {
  if (!record) return undefined;
  const sessionId = record['session_id'] ?? record['id'];
  if (typeof sessionId === 'string' && sessionId !== '') {
    return <SessionInspector sessionId={sessionId} onClose={onClose} />;
  }
  return (
    <InspectorPanel title="Record" onClose={onClose}>
      <KeyValueTree value={record} />
    </InspectorPanel>
  );
}
