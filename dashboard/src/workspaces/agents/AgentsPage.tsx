import { z } from 'zod';
import { OverviewCard, OverviewGrid } from '../../ui/archetypes/OverviewGrid';
import { LegacyBoundary, StatTile } from '../../ui/LegacyStates.tsx';
import { ActivityColumns } from '../../ui/ActivityColumns.tsx';
import { AnyObject } from '../../data/query/legacy.ts';
import { useLegacy } from '../../data/query/useLegacy.ts';

const BASE = '/api/plugins/analytics';

const UsagePayload = z
  .object({
    available: z.boolean(),
    event_count: z.number().optional(),
    by_category: z
      .array(
        z
          .object({ kind: z.string(), category: z.string(), events: z.number() })
          .passthrough(),
      )
      .optional(),
  })
  .passthrough();

const HintsPayload = z
  .object({
    available: z.boolean().optional(),
    families: z.array(AnyObject).optional(),
  })
  .passthrough();

/** Agents: how connected agents actually use TraceDecay — tool-usage
 * composition from analytics events plus under-used tool families the hint
 * engine is nudging. Per-agent session drill-down lands with the Loom span
 * sources. */
export function AgentsPage() {
  const usage = useLegacy(['analytics', 'usage'], `${BASE}/usage`, UsagePayload);
  const hints = useLegacy(['analytics', 'hints'], `${BASE}/hints`, HintsPayload);

  return (
    <LegacyBoundary title="Agents" pending={usage.isPending} result={usage.data}>
      {(data) => {
        const rows = [...(data.by_category ?? [])].sort((a, b) => b.events - a.events);
        const buckets = rows.slice(0, 16).map((row) => ({
          label: `${row.kind} · ${row.category}`,
          value: row.events,
          hint: 'events',
        }));
        return (
          <div className="flex h-full flex-col overflow-auto">
            <div className="flex items-center gap-3 border-b border-edge-subtle px-4 py-2">
              <h1 className="text-sm font-semibold tracking-tight">Agents</h1>
              <span className="text-2xs text-text-muted">
                tool-usage analytics{data.available ? '' : ' · store unavailable'}
              </span>
            </div>
            <div className="grid grid-cols-2 gap-3 p-4 md:grid-cols-4">
              <StatTile
                label="events"
                value={(data.event_count ?? 0).toLocaleString()}
              />
              <StatTile label="categories" value={rows.length} />
            </div>
            <OverviewGrid>
              <OverviewCard title="Usage by category">
                {buckets.length > 0 ? (
                  <ActivityColumns buckets={buckets} height={56} />
                ) : (
                  <p className="text-2xs text-text-muted">no analytics events recorded</p>
                )}
              </OverviewCard>
              <OverviewCard title="Top categories">
                {rows.length === 0 ? (
                  <p className="text-2xs text-text-muted">no usage recorded</p>
                ) : (
                  <div className="flex flex-col gap-1">
                    {rows.slice(0, 10).map((row) => (
                      <div
                        key={`${row.kind}:${row.category}`}
                        className="flex items-baseline gap-2 text-xs"
                      >
                        <span className="w-16 shrink-0 text-2xs text-text-muted">
                          {row.kind}
                        </span>
                        <span className="min-w-0 flex-1 truncate">{row.category}</span>
                        <span className="tabular shrink-0 text-2xs text-text-muted">
                          {row.events.toLocaleString()}
                        </span>
                      </div>
                    ))}
                  </div>
                )}
              </OverviewCard>
              <OverviewCard title="Under-used tool families">
                <LegacyBoundary title="Hints" pending={hints.isPending} result={hints.data}>
                  {(hintData) => {
                    const families = hintData.families ?? [];
                    if (families.length === 0)
                      return (
                        <p className="text-2xs text-text-muted">
                          no under-used families flagged
                        </p>
                      );
                    return (
                      <div className="flex flex-col gap-1">
                        {families.slice(0, 10).map((family, i) => (
                          <p key={i} className="truncate text-xs">
                            {String(
                              family['family'] ?? family['name'] ?? family['category'] ?? i,
                            )}
                          </p>
                        ))}
                      </div>
                    );
                  }}
                </LegacyBoundary>
              </OverviewCard>
            </OverviewGrid>
          </div>
        );
      }}
    </LegacyBoundary>
  );
}
