import { z } from 'zod';
import { OverviewCard, OverviewGrid } from '../../ui/archetypes/OverviewGrid';
import { KeyValueTree } from '../../ui/archetypes/ExplorerSplit.tsx';
import { LegacyBoundary, StatTile } from '../../ui/LegacyStates.tsx';
import { AnyObject } from '../../data/query/legacy.ts';
import { useLegacy } from '../../data/query/useLegacy.ts';

const BASE = '/api/plugins/savings';

const ModelsPayload = z
  .object({ models: z.array(AnyObject).optional(), items: z.array(AnyObject).optional() })
  .passthrough();

/** Costs: savings ledger, model usage, pricing. ECharts series land with the
 * charting phase; truthful numbers ship first. */
export function CostsPage() {
  const overview = useLegacy(['savings', 'overview'], `${BASE}/overview`, AnyObject);
  const models = useLegacy(['savings', 'models'], `${BASE}/models`, ModelsPayload);

  return (
    <LegacyBoundary title="Costs" pending={overview.isPending} result={overview.data}>
      {(data) => (
        <div className="flex h-full flex-col overflow-auto">
          <div className="flex items-center gap-3 border-b border-edge-subtle px-4 py-2">
            <h1 className="text-sm font-semibold tracking-tight">Costs</h1>
          </div>
          <div className="grid grid-cols-2 gap-3 p-4 md:grid-cols-4">
            {Object.entries(data)
              .filter(([, v]) => typeof v === 'number' || typeof v === 'string')
              .slice(0, 8)
              .map(([k, v]) => (
                <StatTile key={k} label={k.replaceAll('_', ' ')} value={String(v)} />
              ))}
          </div>
          <OverviewGrid>
            <OverviewCard title="Models">
              {models.data?.outcome === 'ok' ? (
                <KeyValueTree value={models.data.data.models ?? models.data.data.items ?? []} />
              ) : (
                <p className="text-2xs text-text-muted">model breakdown unavailable</p>
              )}
            </OverviewCard>
          </OverviewGrid>
        </div>
      )}
    </LegacyBoundary>
  );
}
