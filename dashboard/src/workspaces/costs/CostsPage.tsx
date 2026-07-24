import { OverviewCard, OverviewGrid } from '../../ui/archetypes/OverviewGrid';
import { Chart } from '../../viz/chart/Chart.tsx';
import { LegacyBoundary, StatTile } from '../../ui/LegacyStates.tsx';
import { useLegacy } from '../../data/query/useLegacy.ts';
import { SavingsOverviewPayloadSchema } from './contracts.ts';

const BASE = '/api/plugins/savings';

/** Costs: savings ledger over four windows, actual turn spend, and the
 * per-project lifetime savings distribution. ECharts series land with the
 * charting phase; truthful numbers ship first. */
export function CostsPage() {
  const overview = useLegacy(
    ['savings', 'overview'],
    `${BASE}/overview`,
    SavingsOverviewPayloadSchema,
  );

  return (
    <LegacyBoundary title="Costs" pending={overview.isPending} result={overview.data}>
      {(data) => {
        const ledger = data.savings.available ? data.savings.ledger : undefined;
        const lifetime = data.savings.lifetime_counters;
        const projects = [...(lifetime?.projects ?? [])]
          .filter((p) => (p.tokens_saved ?? 0) > 0)
          .sort((a, b) => (b.tokens_saved ?? 0) - (a.tokens_saved ?? 0))
          .slice(0, 12);
        const projectMax = projects[0]?.tokens_saved ?? 1;
        return (
          <div className="flex h-full flex-col overflow-auto">
            <div className="flex items-center gap-3 border-b border-edge-subtle px-4 py-2">
              <h1 className="text-sm font-semibold tracking-tight">Costs</h1>
              <span className="text-2xs text-text-muted">
                {data.turns.available && data.turns.cost_basis
                  ? `cost basis: ${data.turns.cost_basis}`
                  : 'turn ledger unavailable'}
              </span>
            </div>
            <div className="grid grid-cols-2 gap-3 p-4 md:grid-cols-4">
              <StatTile
                label="saved today"
                value={ledger ? formatTokens(ledger.today.saved_tokens) : '—'}
              />
              <StatTile
                label="saved 7d"
                value={ledger ? formatTokens(ledger.last_7d.saved_tokens) : '—'}
              />
              <StatTile
                label="saved 30d"
                value={ledger ? formatTokens(ledger.last_30d.saved_tokens) : '—'}
              />
              <StatTile
                label="saved all-time"
                value={ledger ? formatTokens(ledger.all_time.saved_tokens) : '—'}
              />
            </div>
            <OverviewGrid>
              <OverviewCard title="Savings by window">
                {ledger ? (
                  <Chart
                    ariaLabel="Saved tokens across today, last 7 days, last 30 days, and all time; the stat tiles above carry the exact values"
                    height={180}
                    option={{
                      xAxis: {
                        type: 'category',
                        data: ['today', '7d', '30d', 'all time'],
                      },
                      yAxis: { type: 'value' },
                      series: [
                        {
                          type: 'bar',
                          barWidth: 22,
                          itemStyle: { borderRadius: [3, 3, 0, 0] },
                          data: [
                            ledger.today.saved_tokens,
                            ledger.last_7d.saved_tokens,
                            ledger.last_30d.saved_tokens,
                            ledger.all_time.saved_tokens,
                          ],
                        },
                      ],
                    }}
                  />
                ) : (
                  <p className="text-2xs text-text-muted">ledger unavailable</p>
                )}
              </OverviewCard>
              <OverviewCard title="Actual spend (turn ledger)">
                {data.turns.available ? (
                  <dl className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs tabular">
                    <dt className="text-text-muted">turns</dt>
                    <dd data-cell="numeric">{(data.turns.turn_count ?? 0).toLocaleString()}</dd>
                    <dt className="text-text-muted">total tokens</dt>
                    <dd data-cell="numeric">{formatTokens(data.turns.total_tokens ?? 0)}</dd>
                    <dt className="text-text-muted">total cost</dt>
                    <dd data-cell="numeric">
                      {data.turns.total_cost_usd != null
                        ? `$${data.turns.total_cost_usd.toFixed(2)}`
                        : '—'}
                    </dd>
                  </dl>
                ) : (
                  <p className="text-2xs text-text-muted">turn ledger unavailable</p>
                )}
              </OverviewCard>
              <OverviewCard title="Savings by project (lifetime)">
                {projects.length > 0 ? (
                  <div className="flex flex-col gap-1.5">
                    {projects.map((project, i) => (
                      <div key={`${project.path ?? i}`} className="flex items-center gap-2">
                        <span
                          className="min-w-0 flex-1 truncate font-mono text-2xs text-text-secondary"
                          title={project.path ?? ''}
                        >
                          {shortPath(project.path ?? '')}
                        </span>
                        <span className="relative h-1 w-24 shrink-0 overflow-hidden rounded-full bg-surface-3">
                          <span
                            className="absolute inset-y-0 left-0 rounded-full bg-accent/70"
                            style={{
                              width: `${((project.tokens_saved ?? 0) / projectMax) * 100}%`,
                            }}
                          />
                        </span>
                        <span className="tabular w-16 shrink-0 text-right text-2xs text-text-muted">
                          {formatTokens(project.tokens_saved ?? 0)}
                        </span>
                      </div>
                    ))}
                  </div>
                ) : (
                  <p className="text-2xs text-text-muted">no per-project savings recorded</p>
                )}
              </OverviewCard>
              <OverviewCard title="Pricing">
                <dl className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs">
                  <dt className="text-text-muted">source</dt>
                  <dd>{String(data.pricing.source ?? '—')}</dd>
                  <dt className="text-text-muted">models priced</dt>
                  <dd className="tabular">{String(data.pricing.model_count ?? '—')}</dd>
                  <dt className="text-text-muted">offline</dt>
                  <dd>{String(data.pricing.offline ?? '—')}</dd>
                </dl>
              </OverviewCard>
            </OverviewGrid>
          </div>
        );
      }}
    </LegacyBoundary>
  );
}

function formatTokens(tokens: number): string {
  if (tokens >= 1_000_000_000) return `${(tokens / 1_000_000_000).toFixed(1)}B`;
  if (tokens >= 1_000_000) return `${(tokens / 1_000_000).toFixed(1)}M`;
  if (tokens >= 1_000) return `${(tokens / 1_000).toFixed(1)}k`;
  return tokens.toLocaleString();
}

function shortPath(path: string): string {
  const parts = path.split('/').filter(Boolean);
  return parts.slice(-2).join('/') || path;
}
