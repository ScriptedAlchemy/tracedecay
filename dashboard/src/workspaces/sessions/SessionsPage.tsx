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

const BASE = '/api/plugins/hermes-lcm';

const SessionsPayload = z
  .object({
    sessions: z.array(AnyObject).optional(),
    items: z.array(AnyObject).optional(),
  })
  .passthrough();

/** Sessions: LCM store — overview stats + session list + drill-down. */
export function SessionsPage() {
  const overview = useLegacy(['lcm', 'overview'], `${BASE}/overview`, AnyObject);
  const timeline = useLegacy(['lcm', 'timeline'], `${BASE}/timeline`, SessionsPayload);
  const [selected, setSelected] = useState<Record<string, unknown> | null>(null);

  return (
    <ExplorerSplit
      filters={
        <LegacyBoundary title="LCM" pending={overview.isPending} result={overview.data}>
          {(data) => (
            <div className="flex flex-col gap-2">
              {Object.entries(data)
                .filter(([, v]) => typeof v === 'number' || typeof v === 'string')
                .slice(0, 8)
                .map(([k, v]) => (
                  <StatTile key={k} label={k.replaceAll('_', ' ')} value={String(v)} />
                ))}
            </div>
          )}
        </LegacyBoundary>
      }
      list={
        <LegacyBoundary title="Sessions" pending={timeline.isPending} result={timeline.data}>
          {(data) => {
            const rows = data.sessions ?? data.items ?? [];
            if (rows.length === 0)
              return (
                <p className="p-6 text-center text-sm text-text-muted">
                  no sessions in the current window
                </p>
              );
            return (
              <div>
                {rows.map((row, i) => {
                  const id = String(row['session_id'] ?? row['id'] ?? i);
                  const provider = String(row['provider'] ?? row['source'] ?? '');
                  const when = String(row['started_at'] ?? row['timestamp'] ?? '');
                  return (
                    <DataRow
                      key={id}
                      selected={selected === row}
                      onSelect={() => setSelected(row)}
                    >
                      <span className="w-24 shrink-0 truncate text-text-muted">{provider}</span>
                      <span className="min-w-0 flex-1 truncate">{id}</span>
                      <span className="tabular shrink-0 text-2xs text-text-muted">{when}</span>
                    </DataRow>
                  );
                })}
              </div>
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
