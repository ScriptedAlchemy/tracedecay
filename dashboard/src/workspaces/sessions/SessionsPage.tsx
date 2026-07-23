import { useState } from 'react';
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

/** Sessions: LCM store — overview stats + session list + drill-down. */
export function SessionsPage() {
  const overview = useLegacy(['lcm', 'overview'], `${BASE}/overview`, OverviewPayload);
  const timeline = useLegacy(['lcm', 'timeline'], `${BASE}/timeline`, TimelinePayload);
  const [selected, setSelected] = useState<Record<string, unknown> | null>(null);

  return (
    <ExplorerSplit
      filters={
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
      }
      list={
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
                      <span className="tabular shrink-0 text-2xs text-text-muted">{when}</span>
                    </DataRow>
                  );
                }}
              />
            );
          }}
        </LegacyBoundary>
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
