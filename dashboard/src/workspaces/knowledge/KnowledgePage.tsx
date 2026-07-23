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

const BASE = '/api/plugins/holographic';

const OverviewPayload = z
  .object({ facts: z.array(AnyObject).optional(), items: z.array(AnyObject).optional() })
  .passthrough();

/** Knowledge: memory facts, evidence, curation status. Semantic map (WebGL
 * scatter) is the phase-2 canvas per the visualization catalog. */
export function KnowledgePage() {
  const status = useLegacy(['memory', 'status'], `${BASE}/status`, AnyObject);
  const overview = useLegacy(['memory', 'overview'], `${BASE}/`, OverviewPayload);
  const [selected, setSelected] = useState<Record<string, unknown> | null>(null);

  return (
    <ExplorerSplit
      filters={
        <LegacyBoundary title="Memory" pending={status.isPending} result={status.data}>
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
        <LegacyBoundary title="Knowledge" pending={overview.isPending} result={overview.data}>
          {(data) => {
            const rows = data.facts ?? data.items ?? [];
            if (rows.length === 0)
              return (
                <p className="p-6 text-center text-sm text-text-muted">
                  no facts surfaced by the overview
                </p>
              );
            return (
              <div>
                {rows.map((row, i) => {
                  const id = String(row['fact_id'] ?? row['id'] ?? i);
                  const text = String(row['summary'] ?? row['text'] ?? row['content'] ?? id);
                  const trust = row['trust'] ?? row['confidence'];
                  return (
                    <DataRow key={id} selected={selected === row} onSelect={() => setSelected(row)}>
                      <span className="min-w-0 flex-1 truncate">{text}</span>
                      {trust !== undefined ? (
                        <span className="tabular shrink-0 text-2xs text-text-muted">
                          trust {String(trust)}
                        </span>
                      ) : null}
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
          <InspectorPanel title="Fact" onClose={() => setSelected(null)}>
            <KeyValueTree value={selected} />
          </InspectorPanel>
        ) : undefined
      }
    />
  );
}
