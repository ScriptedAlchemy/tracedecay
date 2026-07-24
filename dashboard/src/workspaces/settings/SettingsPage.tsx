import { KeyValueTree } from '../../ui/archetypes/ExplorerSplit.tsx';
import { LegacyBoundary } from '../../ui/LegacyStates.tsx';
import { AnyObject } from '../../data/query/legacy.ts';
import { useLegacy } from '../../data/query/useLegacy.ts';
import { OverviewCard, OverviewGrid } from '../../ui/archetypes/OverviewGrid';

/** Settings: effective layered configuration (read-only first; typed patch
 * preview/validate/CAS lands with the config-surface phase). */
export function SettingsPage() {
  const settings = useLegacy(['settings'], '/api/settings', AnyObject);

  return (
    <div
      className="flex h-full flex-col overflow-auto"
      tabIndex={0}
      role="region"
      aria-label="Settings content"
    >
      <div className="flex items-center gap-3 border-b border-edge-subtle px-4 py-2">
        <h1 className="text-sm font-semibold tracking-tight">Settings</h1>
        <span className="text-2xs text-text-muted">effective configuration · read-only</span>
      </div>
      <OverviewGrid className="grid-cols-1">
        <OverviewCard title="Effective configuration">
          <LegacyBoundary title="Settings" pending={settings.isPending} result={settings.data}>
            {(data) => <KeyValueTree value={data} />}
          </LegacyBoundary>
        </OverviewCard>
      </OverviewGrid>
    </div>
  );
}
